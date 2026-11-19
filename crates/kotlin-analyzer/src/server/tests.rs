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
    server: Option<JoinHandle<Result<super::Ending, String>>>,
    next_id: i32,
    /// Notifications that arrived while waiting for something else.
    unread: Vec<Notification>,
    /// The methods of the requests the server sent.
    server_requests: Vec<String>,
}

impl Client {
    fn start(root: &Path) -> Client {
        Client::start_with(root, json!({ "gradleExport": false }))
    }

    fn start_with(root: &Path, initialization_options: Value) -> Client {
        let (server, connection) = Connection::memory();
        let thread = std::thread::spawn(move || super::run_on(&server));
        let mut client = Client {
            connection,
            server: Some(thread),
            next_id: 0,
            unread: Vec::new(),
            server_requests: Vec::new(),
        };
        client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": {
                    "workspace": {
                        "semanticTokens": { "refreshSupport": true },
                        "didChangeWatchedFiles": { "dynamicRegistration": true }
                    }
                },
                "initializationOptions": initialization_options,
            }),
        );
        client.notify("initialized", json!({}));
        client
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        match self.request_result(method, params) {
            Ok(result) => result,
            Err(error) => panic!("{method}: {error:?}"),
        }
    }

    /// A request's answer, an error included.
    fn request_result(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, lsp_server::ResponseError> {
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
                    return response.response_result;
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

    /// The server's own requests (progress tokens, refreshes) get an empty
    /// answer.
    fn answer(&mut self, request: Request) {
        self.server_requests.push(request.method.clone());
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
        assert_eq!(self.exit(), super::Ending::AfterShutdown);
    }

    /// Waits until the server has sent a request with `method`.
    fn wait_for_server_request(&mut self, method: &str) {
        while !self.server_requests.iter().any(|sent| sent == method) {
            match self.receive() {
                Message::Request(request) => self.answer(request),
                Message::Notification(notification) => self.unread.push(notification),
                Message::Response(_) => {}
            }
        }
    }

    fn exit(mut self) -> super::Ending {
        self.notify("exit", Value::Null);
        let server = self.server.take().expect("started");
        server
            .join()
            .expect("the server thread ends")
            .expect("cleanly")
    }

    fn open(&self, path: &Path, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri(path),
                    "languageId": "kotlin",
                    "version": 1,
                    "text": text,
                }
            }),
        );
    }

    /// The next diagnostics published for `path`, as their messages.
    fn diagnostics_of(&mut self, path: &Path) -> Vec<String> {
        let published = self.wait_for(|notification| {
            notification.method == "textDocument/publishDiagnostics"
                && notification.params["uri"] == uri(path)
        });
        published.params["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic["message"].as_str().unwrap_or("").to_owned())
            .collect()
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

#[test]
fn an_edit_rechecks_other_open_files_and_exit_without_shutdown_fails() {
    let root = workspace("recheck");
    let sources = root.join("src/main/kotlin/app");
    let maker_text = "package app\nclass Box\nfun make(box: Box) {}\n";
    let user_text = "package app\nfun use() = make(Box())\n";
    std::fs::write(sources.join("Maker.kt"), maker_text).unwrap();
    std::fs::write(sources.join("User.kt"), user_text).unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    client.wait_for_server_request("workspace/semanticTokens/refresh");
    let maker = sources.join("Maker.kt");
    let user = sources.join("User.kt");
    client.open(&maker, maker_text);
    client.open(&user, user_text);
    assert!(client.diagnostics_of(&user).is_empty());

    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(&maker), "version": 2 },
            "contentChanges": [{ "text": "package app\nclass Box\nfun make() {}\n" }],
        }),
    );
    let messages = client.diagnostics_of(&user);
    assert_eq!(messages, vec!["No `make` takes these arguments"]);

    assert_eq!(client.exit(), super::Ending::WithoutShutdown);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn files_changed_on_disk_are_read_again_when_the_client_reports_them() {
    let root = workspace("watch");
    let sources = root.join("src/main/kotlin/app");
    std::fs::write(sources.join("First.kt"), "package app\nclass First\n").unwrap();
    let mut client = Client::start(&root);
    client.wait_for_server_request("client/registerCapability");
    client.wait_for_load(1);

    let added = sources.join("Added.kt");
    std::fs::write(&added, "package app\nclass Added\n").unwrap();
    std::fs::remove_file(sources.join("First.kt")).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [
            { "uri": uri(&added), "type": 1 },
            { "uri": uri(&sources.join("First.kt")), "type": 3 },
        ] }),
    );
    assert_eq!(client.workspace_symbols("Added"), vec!["Added"]);
    assert!(client.workspace_symbols("First").is_empty());

    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_sessions_log_holds_no_code_names_or_paths() {
    crate::log::capture();
    let root = workspace("privacy");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("QuokkaLedger.kt");
    let text = "package app.zebrasecret\nclass QuokkaLedger(val wombatBalance: Long) {\n    fun platypusTotal(): Long = wombatBalance + 7\n}\nval narwhalNote = \"okapi passphrase\"\n";
    std::fs::write(&file, text).unwrap();
    let mut client = Client::start_with(&root, json!({ "gradleExport": false, "log": "debug" }));
    client.wait_for_load(1);
    client.open(&file, text);
    client.diagnostics_of(&file);
    let position = |line: u32, character: u32| {
        json!({
            "textDocument": { "uri": uri(&file) },
            "position": { "line": line, "character": character },
        })
    };
    client.request("textDocument/hover", position(2, 33));
    client.request("textDocument/completion", position(2, 32));
    client.request("textDocument/definition", position(2, 33));
    assert_eq!(
        client.workspace_symbols("QuokkaLedger"),
        vec!["QuokkaLedger"]
    );
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(&file), "version": 2 },
            "contentChanges": [{ "text": text.replace("okapi", "dugongSecret") }],
        }),
    );
    client.diagnostics_of(&file);
    client.shut_down();

    let log = crate::log::captured().join("\n");
    let root_text = root.display().to_string();
    for secret in [
        "zebrasecret",
        "Quokka",
        "wombat",
        "platypus",
        "narwhal",
        "okapi",
        "passphrase",
        "dugong",
        "privacy",
        "src/main",
        root_text.as_str(),
    ] {
        assert!(!log.contains(secret), "`{secret}` in the log:\n{log}");
    }
    for expected in [
        "\"event\":\"session-started\"",
        "\"event\":\"load-finished\"",
        "\"event\":\"document-opened\"",
        "\"event\":\"document-changed\"",
        "\"method\":\"textDocument/hover\"",
        "\"method\":\"workspace/symbol\"",
        "\"event\":\"diagnostics\"",
        "\"waited\":",
    ] {
        assert!(log.contains(expected), "no {expected} in the log:\n{log}");
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_panic_in_a_request_is_answered_and_logged_and_the_server_goes_on() {
    crate::log::capture();
    let root = workspace("panic");
    let sources = root.join("src/main/kotlin/app");
    std::fs::write(sources.join("First.kt"), "package app\nclass First\n").unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);

    let answer = client.request_result(
        "kotlin-analyzer/testPanic",
        json!({ "secret": "ibisVault" }),
    );
    let error = answer.expect_err("a panicking request is answered with an error");
    assert_eq!(error.code, lsp_server::ErrorCode::InternalError as i32);
    assert_eq!(client.workspace_symbols("First"), vec!["First"]);
    client.shut_down();

    let log = crate::log::captured();
    let panic = log
        .iter()
        .find(|line| line.contains("\"event\":\"panic\"") && line.contains("a test panic about"))
        .unwrap_or_else(|| panic!("no panic logged:\n{}", log.join("\n")));
    assert!(!panic.contains("ibisVault"), "{panic}");
    assert!(panic.contains("\"while\":\"request\""), "{panic}");
    assert!(
        panic.contains("crates/kotlin-analyzer/src/server/handlers.rs"),
        "{panic}"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_panic_while_loading_ends_the_load_and_the_server_goes_on() {
    crate::log::capture();
    let root = workspace("loader-panic");
    std::fs::write(root.join("panic-while-loading"), "").unwrap();
    let mut client = Client::start(&root);
    let ended = client.wait_for(|notification| {
        notification.method == "$/progress"
            && notification.params["token"] == "kotlin-analyzer/loading/1"
            && notification.params["value"]["kind"] == "end"
    });
    let message = ended.params["value"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("loading failed"), "{message}");
    assert!(client.workspace_symbols("Anything").is_empty());
    client.shut_down();
    let log = crate::log::captured().join("\n");
    assert!(
        log.contains("\"event\":\"load-failed\"") && log.contains("\"reason\":\"panicked\""),
        "{log}"
    );
    std::fs::remove_dir_all(&root).unwrap();
}
