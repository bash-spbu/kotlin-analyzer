//! The server's main loop, driven over an in-memory connection the way an
//! editor drives it: initialize, load, open a document, reload, shut down.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::{Value, json};

/// How long a test waits for one message before giving up.
const PATIENCE: Duration = Duration::from_secs(30);

/// An editor talking to a server running on its own thread.
struct Client {
    connection: Connection,
    server: Option<JoinHandle<Result<(), String>>>,
    next_id: i32,
    /// Notifications that arrived while waiting for something else.
    unread: Vec<Notification>,
}

impl Client {
    fn start(root: &Path) -> Client {
        let (server, connection) = Connection::memory();
        let thread = std::thread::spawn(move || super::run_on(&server));
        let mut client = Client {
            connection,
            server: Some(thread),
            next_id: 0,
            unread: Vec::new(),
        };
        client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": {},
                "initializationOptions": { "gradleExport": false },
            }),
        );
        client.notify("initialized", json!({}));
        client
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        let request = Request::new(id.clone(), method.to_owned(), params);
        self.connection
            .sender
            .send(Message::Request(request))
            .unwrap();
        loop {
            match self.receive() {
                Message::Response(response) if response.id == id => {
                    return match response.response_result {
                        Ok(result) => result,
                        Err(error) => panic!("{method}: {error:?}"),
                    };
                }
                Message::Response(_) => {}
                Message::Request(request) => self.answer(request),
                Message::Notification(notification) => self.unread.push(notification),
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let notification = Notification::new(method.to_owned(), params);
        self.connection
            .sender
            .send(Message::Notification(notification))
            .unwrap();
    }

    /// The first notification (read or still to come) that `wanted` takes.
    fn wait_for(&mut self, wanted: impl Fn(&Notification) -> bool) -> Notification {
        if let Some(position) = self.unread.iter().position(&wanted) {
            return self.unread.remove(position);
        }
        loop {
            match self.receive() {
                Message::Notification(notification) if wanted(&notification) => {
                    return notification;
                }
                Message::Notification(notification) => self.unread.push(notification),
                Message::Request(request) => self.answer(request),
                Message::Response(_) => {}
            }
        }
    }

    /// Waits until the `generation`-th load has finished.
    fn wait_for_load(&mut self, generation: u64) {
        let token = format!("kotlin-analyzer/loading/{generation}");
        self.wait_for(|notification| {
            notification.method == "$/progress"
                && notification.params["token"] == token
                && notification.params["value"]["kind"] == "end"
        });
    }

    fn receive(&self) -> Message {
        self.connection
            .receiver
            .recv_timeout(PATIENCE)
            .expect("the server answers in time")
    }

    /// The server's own requests (progress tokens) get an empty answer.
    fn answer(&self, request: Request) {
        let response = Response::new_ok(request.id, Value::Null);
        self.connection
            .sender
            .send(Message::Response(response))
            .unwrap();
    }

    fn workspace_symbols(&mut self, query: &str) -> Vec<String> {
        let found = self.request("workspace/symbol", json!({ "query": query }));
        found
            .as_array()
            .map(|symbols| {
                symbols
                    .iter()
                    .filter_map(|symbol| symbol["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn shut_down(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let server = self.server.take().expect("started");
        server
            .join()
            .expect("the server thread ends")
            .expect("cleanly");
    }
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn workspace(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "kotlin-analyzer-server-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/main/kotlin/app")).unwrap();
    root
}

#[test]
fn loads_publishes_diagnostics_and_reloads_new_and_deleted_files() {
    let root = workspace("reload");
    let sources = root.join("src/main/kotlin/app");
    std::fs::write(sources.join("First.kt"), "package app\nclass First\n").unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    assert_eq!(client.workspace_symbols("First"), vec!["First"]);

    let broken = sources.join("Broken.kt");
    client.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri(&broken),
                "languageId": "kotlin",
                "version": 1,
                "text": "package app\nfun broken( {\n",
            }
        }),
    );
    let published = client.wait_for(|notification| {
        notification.method == "textDocument/publishDiagnostics"
            && notification.params["uri"] == uri(&broken)
    });
    assert!(
        !published.params["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{published:?}"
    );

    std::fs::write(sources.join("Second.kt"), "package app\nclass Second\n").unwrap();
    std::fs::remove_file(sources.join("First.kt")).unwrap();
    let reloaded = client.request(
        "workspace/executeCommand",
        json!({ "command": super::RELOAD_COMMAND, "arguments": [] }),
    );
    assert_eq!(reloaded, Value::Null);
    client.wait_for_load(2);
    assert_eq!(client.workspace_symbols("Second"), vec!["Second"]);
    assert!(client.workspace_symbols("First").is_empty());

    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}
