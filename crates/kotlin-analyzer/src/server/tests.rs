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
        Client::start_offering(root, initialization_options, json!({})).0
    }

    /// A client with `offered` added to its capabilities (`general`,
    /// `textDocument`, more of `workspace`), with the server's answer to
    /// `initialize`.
    fn start_offering(
        root: &Path,
        initialization_options: Value,
        offered: Value,
    ) -> (Client, Value) {
        let java = project_model::jdk::JavaEnvironment::current();
        Client::start_with_java(root, initialization_options, offered, java)
    }

    /// A client whose server looks for the JDK in `java`.
    fn start_with_java(
        root: &Path,
        initialization_options: Value,
        offered: Value,
        java: project_model::jdk::JavaEnvironment,
    ) -> (Client, Value) {
        // Agents' sockets only where a test asks for them.
        let mut initialization_options = initialization_options;
        if let Some(options) = initialization_options.as_object_mut() {
            options.entry("agentSockets").or_insert(Value::Bool(false));
        }
        let (server, connection) = Connection::memory();
        let thread = std::thread::spawn(move || super::run_on_with_java(&server, java));
        let mut client = Client {
            connection,
            server: Some(thread),
            next_id: 0,
            unread: Vec::new(),
            server_requests: Vec::new(),
        };
        let mut capabilities = json!({
            "workspace": {
                "semanticTokens": { "refreshSupport": true },
                "didChangeWatchedFiles": { "dynamicRegistration": true }
            }
        });
        for (key, value) in offered.as_object().into_iter().flatten() {
            match (capabilities[key].as_object_mut(), value.as_object()) {
                (Some(existing), Some(added)) => {
                    existing.extend(
                        added
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                _ => capabilities[key] = value.clone(),
            }
        }
        let initialized = client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(root),
                "capabilities": capabilities,
                "initializationOptions": initialization_options,
            }),
        );
        client.notify("initialized", json!({}));
        (client, initialized)
    }

    /// A client of a workspace of several folders.
    fn start_in_folders(folders: &[&Path]) -> Client {
        let (server, connection) = Connection::memory();
        let thread = std::thread::spawn(move || super::run_on(&server));
        let mut client = Client {
            connection,
            server: Some(thread),
            next_id: 0,
            unread: Vec::new(),
            server_requests: Vec::new(),
        };
        let workspace_folders: Vec<Value> = folders
            .iter()
            .map(|folder| json!({ "uri": uri(folder), "name": "folder" }))
            .collect();
        client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": uri(folders[0]),
                "workspaceFolders": workspace_folders,
                "capabilities": {},
                "initializationOptions": { "gradleExport": false, "agentSockets": false },
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

    /// The server's next request with `method`, left for the test to
    /// answer (`reply`); its other requests get an empty answer.
    fn take_server_request(&mut self, method: &str) -> Request {
        loop {
            match self.receive() {
                Message::Request(request) if request.method == method => {
                    self.server_requests.push(request.method.clone());
                    return request;
                }
                Message::Request(request) => self.answer(request),
                Message::Notification(notification) => self.unread.push(notification),
                Message::Response(_) => {}
            }
        }
    }

    fn reply(&self, request: Request, result: Value) {
        let response = Response::new_ok(request.id, result);
        self.connection
            .sender
            .send(Message::Response(response))
            .unwrap();
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

#[test]
fn a_class_name_in_an_inlay_hint_links_to_its_declaration() {
    let root = workspace("hint-links");
    let sources = root.join("src/main/kotlin/app");
    let boxes = sources.join("Box.kt");
    let user = sources.join("User.kt");
    std::fs::write(&boxes, "package app\nclass Box\n").unwrap();
    let user_text = "package app\nfun use() {\n    val made = make()\n}\nfun make() = Box()\n";
    std::fs::write(&user, user_text).unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    client.open(&user, user_text);
    let hints = client.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri(&user) },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 5, "character": 0 } },
        }),
    );
    let made = &hints.as_array().unwrap()[0];
    let parts = made["label"].as_array().expect("the label in parts");
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|part| part["value"].as_str())
        .collect();
    assert_eq!(texts, [": ", "Box"]);
    assert_eq!(parts[1]["location"]["uri"], uri(&boxes));
    assert_eq!(parts[1]["location"]["range"]["start"]["line"], 1);
    assert!(parts[0].get("location").is_none_or(Value::is_null));
    // Nothing to resolve in it: resolving gives it back as it was.
    let resolved = client.request("inlayHint/resolve", made.clone());
    assert_eq!(&resolved, made);
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_cancelled_request_is_answered_cancelled_once_and_the_server_goes_on() {
    let root = workspace("cancel");
    let sources = root.join("src/main/kotlin/app");
    std::fs::write(sources.join("First.kt"), "package app\nclass First\n").unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);

    let slow = RequestId::from(1000);
    client
        .connection
        .sender
        .send(Message::Request(Request::new(
            slow.clone(),
            "kotlin-analyzer/testSlow".to_owned(),
            Value::Null,
        )))
        .unwrap();
    client.notify("$/cancelRequest", json!({ "id": 1000 }));
    let mut answers = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_millis(800);
    while std::time::Instant::now() < deadline {
        match client
            .connection
            .receiver
            .recv_timeout(Duration::from_millis(50))
        {
            Ok(Message::Response(response)) if response.id == slow => answers.push(response),
            Ok(Message::Request(request)) => client.answer(request),
            Ok(Message::Notification(notification)) => client.unread.push(notification),
            _ => {}
        }
    }
    let [answer] = &answers[..] else {
        panic!("one answer to the cancelled request: {answers:?}");
    };
    let error = answer
        .response_result
        .as_ref()
        .expect_err("answered with an error");
    assert_eq!(error.code, lsp_server::ErrorCode::RequestCanceled as i32);
    assert_eq!(client.workspace_symbols("First"), vec!["First"]);
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_incremental_change_edits_the_open_text() {
    let root = workspace("incremental");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("Edited.kt");
    let text = "package app\nfun broken( {\n";
    std::fs::write(&file, text).unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    client.open(&file, text);
    assert!(!client.diagnostics_of(&file).is_empty());
    // `broken( {` → `fixed() {}` in two ranged changes.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(&file), "version": 2 },
            "contentChanges": [
                { "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 11 } }, "text": "fixed()" },
                { "range": { "start": { "line": 1, "character": 13 }, "end": { "line": 1, "character": 13 } }, "text": "}" },
            ],
        }),
    );
    assert!(client.diagnostics_of(&file).is_empty());
    assert_eq!(client.workspace_symbols("fixed"), vec!["fixed"]);
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn semantic_tokens_come_whole_as_a_delta_and_for_a_range() {
    let root = workspace("tokens");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("Tokens.kt");
    let text = "package app\nclass Box(val size: Int)\nfun make(): Box = Box(1)\n";
    std::fs::write(&file, text).unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    client.open(&file, text);
    let document = json!({ "uri": uri(&file) });
    let numbers = |value: &Value| -> Vec<u64> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|number| number.as_u64().unwrap())
            .collect()
    };
    let first = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": document }),
    );
    let first_id = first["resultId"].as_str().expect("a result id").to_owned();
    let mut sent = numbers(&first["data"]);

    // A line inserted after the class: the function's tokens change.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(&file), "version": 2 },
            "contentChanges": [{
                "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 0 } },
                "text": "val made = make()\n",
            }],
        }),
    );
    client.diagnostics_of(&file);
    let delta = client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": document, "previousResultId": first_id }),
    );
    let edits = delta["edits"].as_array().expect("a delta, not the whole");
    for edit in edits.iter().rev() {
        let start = edit["start"].as_u64().unwrap() as usize;
        let deleted = edit["deleteCount"].as_u64().unwrap() as usize;
        let inserted = edit.get("data").map(numbers).unwrap_or_default();
        sent.splice(start..start + deleted, inserted);
    }
    let whole = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": document }),
    );
    assert_eq!(sent, numbers(&whole["data"]));
    // An unknown previous id: everything again.
    let stale = client.request(
        "textDocument/semanticTokens/full/delta",
        json!({ "textDocument": document, "previousResultId": "no-such-id" }),
    );
    assert_eq!(numbers(&stale["data"]), numbers(&whole["data"]));

    // The class's line alone: `Box` and `size` (with no standard library
    // in this workspace, `Int` resolves to nothing: no token).
    let range = client.request(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": document,
            "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 2, "character": 0 } },
        }),
    );
    let in_range = numbers(&range["data"]);
    assert_eq!(in_range.len() / 5, 2, "{in_range:?}");
    assert_eq!(
        in_range[0], 1,
        "the first token's line, from the file's start"
    );
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn renaming_files_and_folders_edits_what_follows_them() {
    let root = workspace("file-rename");
    let kotlin = root.join("src/main/kotlin");
    std::fs::create_dir_all(kotlin.join("other")).unwrap();
    let order = kotlin.join("app/Order.kt");
    let user = kotlin.join("other/Use.kt");
    std::fs::write(&order, "package app\nclass Order\n").unwrap();
    std::fs::write(
        &user,
        "package other\nimport app.Order\nfun use(order: Order) = order\n",
    )
    .unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    // What each file's edits write, in order.
    let written = |edit: &Value, file: &Path| -> Vec<String> {
        edit["documentChanges"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|document| document["textDocument"]["uri"] == uri(file))
            .flat_map(|document| document["edits"].as_array().cloned().unwrap_or_default())
            .map(|edit| edit["newText"].as_str().unwrap().to_owned())
            .collect()
    };

    // The folder `app` renamed `billing`: the package moves with it.
    let moved = client.request(
        "workspace/willRenameFiles",
        json!({ "files": [{ "oldUri": uri(&kotlin.join("app")), "newUri": uri(&kotlin.join("billing")) }] }),
    );
    assert_eq!(written(&moved, &order), ["billing"]);
    assert_eq!(written(&moved, &user), ["billing."]);

    // `Order.kt` renamed `Invoice.kt`: so is its class.
    let renamed = client.request(
        "workspace/willRenameFiles",
        json!({ "files": [{ "oldUri": uri(&order), "newUri": uri(&kotlin.join("app/Invoice.kt")) }] }),
    );
    assert_eq!(written(&renamed, &order), ["Invoice"]);
    assert_eq!(written(&renamed, &user), ["Invoice", "Invoice"]);

    // Once renamed on disk, the old file is gone and the new one read.
    let invoice = kotlin.join("app/Invoice.kt");
    std::fs::rename(&order, &invoice).unwrap();
    std::fs::write(&invoice, "package app\nclass Invoice\n").unwrap();
    client.notify(
        "workspace/didRenameFiles",
        json!({ "files": [{ "oldUri": uri(&order), "newUri": uri(&invoice) }] }),
    );
    assert_eq!(client.workspace_symbols("Invoice"), vec!["Invoice"]);
    assert!(client.workspace_symbols("Order").is_empty());
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn positions_count_utf8_bytes_when_the_client_offers_them() {
    let root = workspace("utf8");
    let file = root.join("src/main/kotlin/app/Names.kt");
    // `ёж` is 2 characters, 4 bytes.
    let text = "package app\nval ёж = 1\nval copy = ёж\n";
    std::fs::write(&file, text).unwrap();
    let (mut client, initialized) = Client::start_offering(
        &root,
        json!({ "gradleExport": false }),
        json!({ "general": { "positionEncodings": ["utf-8", "utf-16"] } }),
    );
    assert_eq!(initialized["capabilities"]["positionEncoding"], "utf-8");
    client.wait_for_load(1);
    client.open(&file, text);
    // `ёж` of `copy = ёж` starts at byte 11 of its line.
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri(&file) },
            "position": { "line": 2, "character": 12 },
        }),
    );
    let target = &definition.as_array().expect("a definition")[0];
    assert_eq!(
        target["range"]["start"],
        json!({ "line": 1, "character": 4 })
    );
    assert_eq!(target["range"]["end"], json!({ "line": 1, "character": 8 }));
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn type_and_call_hierarchies_expand_from_the_items_they_send() {
    let root = workspace("hierarchy");
    let file = root.join("src/main/kotlin/app/Shapes.kt");
    let text = "package app\ninterface Shape\nclass Square : Shape\nfun area(shape: Shape): Int = side()\nfun side(): Int = 1\nfun report() = area(Square())\n";
    std::fs::write(&file, text).unwrap();
    let (mut client, initialized) =
        Client::start_offering(&root, json!({ "gradleExport": false }), json!({}));
    assert_eq!(initialized["capabilities"]["typeHierarchyProvider"], true);
    client.wait_for_load(1);
    let at = |line: u32, character: u32| {
        json!({
            "textDocument": { "uri": uri(&file) },
            "position": { "line": line, "character": character },
        })
    };
    let names = |items: &Value, field: Option<&str>| -> Vec<String> {
        let mut names: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| field.map_or(item, |field| &item[field]))
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    };

    let shape = client.request("textDocument/prepareTypeHierarchy", at(1, 11));
    let shape = shape[0].clone();
    assert_eq!(shape["name"], "Shape");
    let subtypes = client.request("typeHierarchy/subtypes", json!({ "item": shape }));
    assert_eq!(names(&subtypes, None), ["Square"]);
    let square = subtypes[0].clone();
    let supertypes = client.request("typeHierarchy/supertypes", json!({ "item": square }));
    assert_eq!(names(&supertypes, None), ["Shape"]);

    let area = client.request("textDocument/prepareCallHierarchy", at(3, 5));
    let area = area[0].clone();
    assert_eq!(area["name"], "area");
    let incoming = client.request("callHierarchy/incomingCalls", json!({ "item": area }));
    assert_eq!(names(&incoming, Some("from")), ["report"]);
    assert_eq!(
        incoming[0]["fromRanges"][0]["start"],
        json!({ "line": 5, "character": 15 })
    );
    let outgoing = client.request("callHierarchy/outgoingCalls", json!({ "item": area }));
    assert_eq!(names(&outgoing, Some("to")), ["side"]);
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_client_that_pulls_diagnostics_is_answered_and_asked_to_pull_again() {
    let root = workspace("pull");
    let file = root.join("src/main/kotlin/app/Broken.kt");
    let text = "package app\nfun broken( {\n";
    std::fs::write(&file, text).unwrap();
    let (mut client, initialized) = Client::start_offering(
        &root,
        json!({ "gradleExport": false }),
        json!({
            "textDocument": { "diagnostic": { "dynamicRegistration": false } },
            "workspace": { "diagnostics": { "refreshSupport": true } },
        }),
    );
    assert_eq!(
        initialized["capabilities"]["diagnosticProvider"]["workspaceDiagnostics"],
        true
    );
    client.wait_for_load(1);
    client.open(&file, text);
    let document = json!({ "textDocument": { "uri": uri(&file) } });
    let first = client.request("textDocument/diagnostic", document.clone());
    assert_eq!(first["kind"], "full");
    assert!(!first["items"].as_array().unwrap().is_empty(), "{first}");
    let id = first["resultId"].as_str().unwrap().to_owned();

    // Asked again with nothing changed: unchanged.
    let mut again = document.clone();
    again["previousResultId"] = json!(id);
    let unchanged = client.request("textDocument/diagnostic", again.clone());
    assert_eq!(unchanged["kind"], "unchanged");

    // An edit: the client is asked to pull again, and gets the new state.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri(&file), "version": 2 },
            "contentChanges": [{ "text": "package app\nfun fixed() {}\n" }],
        }),
    );
    client.wait_for_server_request("workspace/diagnostic/refresh");
    let fixed = client.request("textDocument/diagnostic", again);
    assert_eq!(fixed["kind"], "full");
    assert!(fixed["items"].as_array().unwrap().is_empty(), "{fixed}");

    let workspace = client.request("workspace/diagnostic", json!({ "previousResultIds": [] }));
    let reported: Vec<&str> = workspace["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["uri"].as_str())
        .collect();
    assert!(reported.contains(&uri(&file).as_str()), "{workspace}");
    // No diagnostics were pushed to a client that pulls.
    assert!(
        !client
            .unread
            .iter()
            .any(|notification| notification.method == "textDocument/publishDiagnostics")
    );
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn several_workspace_folders_are_one_workspace_that_follows_their_changes() {
    let billing = workspace("folder-billing");
    let users = workspace("folder-users");
    let reports = workspace("folder-reports");
    std::fs::write(
        billing.join("src/main/kotlin/app/Invoice.kt"),
        "package app\nclass Invoice\n",
    )
    .unwrap();
    std::fs::write(
        users.join("src/main/kotlin/app/Customer.kt"),
        "package app\nclass Customer\n",
    )
    .unwrap();
    std::fs::write(
        reports.join("src/main/kotlin/app/Report.kt"),
        "package app\nclass Report\n",
    )
    .unwrap();
    let mut client = Client::start_in_folders(&[&billing, &users]);
    client.wait_for_load(1);
    assert_eq!(client.workspace_symbols("Invoice"), vec!["Invoice"]);
    assert_eq!(client.workspace_symbols("Customer"), vec!["Customer"]);

    let folder = |path: &Path| json!({ "uri": uri(path), "name": "folder" });
    client.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({ "event": { "added": [folder(&reports)], "removed": [folder(&users)] } }),
    );
    client.wait_for_load(2);
    assert_eq!(client.workspace_symbols("Report"), vec!["Report"]);
    assert!(client.workspace_symbols("Customer").is_empty());
    assert_eq!(client.workspace_symbols("Invoice"), vec!["Invoice"]);
    client.shut_down();
    for root in [billing, users, reports] {
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// An executable script at `path` that runs `body` (`/bin/sh`).
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn formatting_runs_the_configured_formatter_and_shows_its_complaints_only_to_the_user() {
    crate::log::capture();
    let root = workspace("plumbing");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("Main.kt");
    let text = "package app\nfun main() {\nval answer=42\n}\n";
    std::fs::write(&file, text).unwrap();
    let java = sources.join("Legacy.java");
    std::fs::write(&java, "class Legacy {}\n").unwrap();
    // Remembers its arguments; indents the line with `=` and spaces it out.
    let formatter = root.join("fake-ktfmt");
    let arguments = root.join("arguments");
    script(
        &formatter,
        &format!(
            "echo \"$@\" > {}\nsed 's/^val answer=42$/    val answer = 42/'",
            arguments.display()
        ),
    );
    let mut client = Client::start_with(
        &root,
        json!({
            "gradleExport": false,
            "formatting": { "formatter": "ktfmt", "style": "google", "path": formatter },
        }),
    );
    client.wait_for_load(1);
    client.open(&file, text);
    let format = |client: &mut Client, path: &Path| {
        client.request_result(
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": uri(path) },
                "options": { "tabSize": 4, "insertSpaces": true },
            }),
        )
    };
    let edits = format(&mut client, &file).unwrap();
    assert_eq!(
        edits,
        json!([{
            "range": {
                "start": { "line": 2, "character": 0 },
                "end": { "line": 2, "character": 11 },
            },
            "newText": "    val answer = ",
        }])
    );
    assert_eq!(
        std::fs::read_to_string(&arguments).unwrap(),
        "--google-style -\n"
    );
    // Java files aren't formatted.
    client.open(&java, "class Legacy {}\n");
    assert_eq!(format(&mut client, &java).unwrap(), Value::Null);

    // A file the formatter refuses: its complaint, which may quote the
    // code, is the answer's message and not in the log.
    let rejecting = root.join("rejecting-ktrs");
    script(
        &rejecting,
        "echo 'Main.kt:3:1: expecting manateeSecret' >&2\nexit 2",
    );
    client.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "kotlin-analyzer": { "formatting": { "path": rejecting } } } }),
    );
    let error = format(&mut client, &file).unwrap_err();
    assert_eq!(error.code, lsp_server::ErrorCode::RequestFailed as i32);
    assert!(
        error
            .message
            .contains("ktrs couldn't format this file:\nMain.kt:3:1: expecting manateeSecret"),
        "{}",
        error.message
    );
    client.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "formatting": { "formatter": "off" } } }),
    );
    assert_eq!(format(&mut client, &file).unwrap(), Value::Null);
    client.shut_down();

    let log = crate::log::captured().join("\n");
    assert!(!log.contains("manatee"), "the complaint in the log:\n{log}");
    assert!(!log.contains("plumbing"), "a path in the log:\n{log}");
    for expected in [
        "\"event\":\"formatted\"",
        "\"outcome\":\"rejected\"",
        "\"exit-code\":2",
        "\"event\":\"formatting-changed\"",
    ] {
        assert!(log.contains(expected), "no {expected} in the log:\n{log}");
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn code_lenses_run_tests_and_count_references_when_resolved_and_selection_grows() {
    let root = workspace("lenses");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("PriceTest.kt");
    let text = "package app\n\nimport org.junit.jupiter.api.Test\n\nclass PriceTest {\n    @Test\n    fun doubles() { check(price(2)) }\n}\n\nfun price(amount: Int) = amount * 2\nfun check(value: Int) = price(value)\n";
    std::fs::write(&file, text).unwrap();
    let (mut client, initialized) = Client::start_offering(
        &root,
        json!({ "gradleExport": false, "codeLens": { "references": true } }),
        json!({ "workspace": { "codeLens": { "refreshSupport": true } } }),
    );
    assert_eq!(
        initialized["capabilities"]["codeLensProvider"]["resolveProvider"],
        true
    );
    assert_eq!(initialized["capabilities"]["selectionRangeProvider"], true);
    client.wait_for_load(1);
    client.wait_for_server_request("workspace/codeLens/refresh");
    client.open(&file, text);
    let document = json!({ "textDocument": { "uri": uri(&file) } });
    let lenses = client.request("textDocument/codeLens", document.clone());
    let lenses = lenses.as_array().unwrap();
    let run_test = lenses
        .iter()
        .find(|lens| lens["command"]["title"] == "▶ Run test")
        .unwrap();
    assert_eq!(
        run_test["range"]["start"],
        json!({ "line": 6, "character": 8 })
    );
    assert_eq!(
        run_test["command"],
        json!({
            "title": "▶ Run test",
            "command": "kotlin-analyzer.runTests",
            "arguments": [{ "uri": uri(&file), "filter": "app.PriceTest.doubles" }],
        })
    );
    assert!(
        lenses
            .iter()
            .any(|lens| lens["command"]["arguments"][0]["filter"] == "app.PriceTest")
    );
    let price = lenses
        .iter()
        .find(|lens| lens["range"]["start"] == json!({ "line": 9, "character": 4 }))
        .unwrap();
    assert!(
        price.get("command").is_none_or(Value::is_null),
        "counted only on resolve"
    );
    let resolved = client.request("codeLens/resolve", price.clone());
    assert_eq!(resolved["command"]["title"], "2 references");
    assert_eq!(
        resolved["command"]["command"],
        "kotlin-analyzer.showReferences"
    );
    assert_eq!(resolved["command"]["arguments"][0], uri(&file));
    assert_eq!(
        resolved["command"]["arguments"][1],
        json!({ "line": 9, "character": 4 })
    );
    assert_eq!(
        resolved["command"]["arguments"][2]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Tests' lenses turned off: the reference counts stay, the client is
    // asked to refresh.
    client.server_requests.clear();
    client.notify(
        "workspace/didChangeConfiguration",
        json!({ "settings": { "kotlin-analyzer": { "codeLens": { "tests": false } } } }),
    );
    client.wait_for_server_request("workspace/codeLens/refresh");
    let lenses = client.request("textDocument/codeLens", document);
    assert!(
        lenses
            .as_array()
            .unwrap()
            .iter()
            .all(|lens| lens["data"]["offset"].is_u64())
    );

    // From `amount` in `amount * 2`: the word, the expression, the body…
    let selection = client.request(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": uri(&file) },
            "positions": [{ "line": 9, "character": 27 }],
        }),
    );
    let innermost = &selection[0];
    assert_eq!(
        innermost["range"],
        json!({ "start": { "line": 9, "character": 25 }, "end": { "line": 9, "character": 31 } })
    );
    assert_eq!(
        innermost["parent"]["range"],
        json!({ "start": { "line": 9, "character": 25 }, "end": { "line": 9, "character": 35 } })
    );
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn an_agent_asks_the_editors_server_on_its_socket_and_sees_unsaved_text() {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;

    use crate::agent::protocol::{self, AgentQuery, AgentReply, AgentRequest, SourcePosition};

    let root = workspace("agent-socket");
    let sockets = std::env::temp_dir().join(format!("ka-sockets-{}", std::process::id()));
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("Ledger.kt");
    std::fs::write(&file, "package app\nfun total() = 1\nfun use() = total()\n").unwrap();
    let client = Client::start_with(
        &root,
        json!({ "gradleExport": false, "agentSockets": sockets }),
    );
    let socket = protocol::socket_path(&sockets, &root);
    // Opened right after `initialize` is answered.
    let deadline = std::time::Instant::now() + PATIENCE;
    while !socket.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // The socket's directory is only its owner's.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&sockets).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    // Unsaved in the editor: a second use.
    client.open(
        &file,
        "package app\nfun total() = 1\nfun use() = total()\nfun again() = total()\n",
    );
    let stream = UnixStream::connect(&socket).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut ask = |id: u64, query: AgentQuery| -> AgentReply {
        protocol::write_line(&mut writer, &AgentRequest { id, query }).unwrap();
        protocol::read_line::<AgentReply>(&mut reader)
            .unwrap()
            .unwrap()
    };
    // Asked before the load ends, answered after it.
    let references = ask(
        1,
        AgentQuery::References {
            at: SourcePosition {
                file: file.clone(),
                line: 2,
                symbol: Some("total".to_owned()),
                column: None,
            },
        },
    );
    assert_eq!(references.id, 1);
    let answer = references.answer.unwrap();
    assert!(answer.starts_with("2 references\n"), "{answer}");
    assert!(
        answer.contains("src/main/kotlin/app/Ledger.kt:4:15: fun again() = total()"),
        "{answer}"
    );
    let outside = ask(
        2,
        AgentQuery::Outline {
            file: std::env::temp_dir().join("Elsewhere.kt"),
        },
    );
    assert!(
        outside
            .error
            .unwrap()
            .contains("isn't a Kotlin or Java file of the project")
    );
    client.shut_down();
    // Gone with the server.
    std::thread::sleep(Duration::from_millis(100));
    assert!(!socket.exists());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn loading_reports_each_step_in_the_status_bar() {
    let root = workspace("load-steps");
    std::fs::write(
        root.join("src/main/kotlin/app/Order.kt"),
        "package app\nclass Order\n",
    )
    .unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    let token = "kotlin-analyzer/loading/1";
    let steps: Vec<String> = client
        .unread
        .iter()
        .filter(|notification| {
            notification.method == "$/progress" && notification.params["token"] == token
        })
        .map(|notification| {
            let value = &notification.params["value"];
            format!(
                "{}: {}",
                value["kind"].as_str().unwrap_or_default(),
                value["message"].as_str().unwrap_or_default()
            )
        })
        .collect();
    let expected_start = [
        "report: Reading the project model",
        "report: Reading 1 files",
    ];
    assert!(
        steps[1..3] == expected_start,
        "the loading's steps: {steps:?}"
    );
    assert!(steps[0].starts_with("begin: "), "{steps:?}");
    let begin = client
        .unread
        .iter()
        .find(|notification| {
            notification.params["token"] == token && notification.params["value"]["kind"] == "begin"
        })
        .unwrap();
    // Named after the server: the status bar shows a title as it is.
    assert_eq!(
        begin.params["value"]["title"],
        "kotlin-analyzer: Loading workspace"
    );
    // No libraries: no step for them (the end was read waiting for it).
    assert_eq!(steps.len(), 3, "{steps:?}");
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn once_loading_and_checking_are_over_the_status_bar_says_all_is_set_for_a_moment() {
    let root = workspace("all-set");
    std::fs::write(
        root.join("src/main/kotlin/app/Order.kt"),
        "package app\nclass Order\n",
    )
    .unwrap();
    let (mut client, _) = Client::start_offering(
        &root,
        json!({ "gradleExport": false }),
        json!({ "textDocument": { "diagnostic": {} } }),
    );
    client.wait_for_load(1);
    // The workspace check the client asks for right after the load comes
    // before "All set".
    client.request("workspace/diagnostic", json!({ "previousResultIds": [] }));
    let token = "kotlin-analyzer/ready/1";
    let begin = client.wait_for(|notification| {
        notification.method == "$/progress" && notification.params["token"] == token
    });
    assert_eq!(begin.params["value"]["kind"], "begin");
    assert_eq!(begin.params["value"]["title"], "kotlin-analyzer: All set");
    assert_eq!(
        begin.params["value"]["message"],
        "1 files, nothing left to wait for"
    );
    let end = client.wait_for(|notification| {
        notification.method == "$/progress" && notification.params["token"] == token
    });
    assert_eq!(end.params["value"]["kind"], "end");
    // Once per load: another check doesn't say it again.
    client.request("workspace/diagnostic", json!({ "previousResultIds": [] }));
    std::thread::sleep(Duration::from_millis(100));
    client.request("workspace/symbol", json!({ "query": "Order" }));
    assert!(
        !client.unread.iter().any(|notification| {
            notification.params["token"]
                .as_str()
                .is_some_and(|token| token.starts_with("kotlin-analyzer/ready/"))
        }),
        "said twice"
    );
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

/// A machine without a JDK: no `JAVA_HOME`, no `java` on `PATH`, an empty
/// home.
fn no_jdk(root: &Path) -> project_model::jdk::JavaEnvironment {
    project_model::jdk::JavaEnvironment {
        java_home: None,
        path: Vec::new(),
        home: Some(root.join("home")),
        macos_stub: None,
    }
}

/// The titles of a `window/showMessageRequest`'s buttons.
fn button_titles(request: &Request) -> Vec<String> {
    request.params["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|action| action["title"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn without_a_jdk_the_server_asks_which_vendor_then_which_release() {
    let root = workspace("no-jdk");
    std::fs::write(root.join("Main.kt"), "fun main() {}\n").unwrap();
    let (mut client, _) = Client::start_with_java(
        &root,
        json!({ "gradleExport": false }),
        json!({}),
        no_jdk(&root),
    );
    let vendors = client.take_server_request("window/showMessageRequest");
    assert!(
        vendors.params["message"]
            .as_str()
            .unwrap()
            .contains("found no JDK"),
        "{}",
        vendors.params
    );
    assert_eq!(
        button_titles(&vendors),
        [
            "Eclipse Temurin",
            "Azul Zulu",
            "Amazon Corretto",
            "BellSoft Liberica",
            "GraalVM Community",
            "Microsoft",
            "Oracle OpenJDK",
            "Not now"
        ]
    );
    client.reply(vendors, json!({ "title": "Azul Zulu" }));
    let releases = client.take_server_request("window/showMessageRequest");
    assert_eq!(
        releases.params["message"],
        "Which Java version of Azul Zulu?"
    );
    assert_eq!(
        button_titles(&releases),
        ["21 (LTS)", "25 (LTS)", "17 (LTS)", "Cancel"]
    );
    client.reply(releases, json!({ "title": "Cancel" }));
    client.wait_for_load(1);

    // Code actions offer it anywhere while there's no JDK.
    let file = root.join("Main.kt");
    client.open(&file, "fun main() {}\n");
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri(&file) },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            "context": { "diagnostics": [] },
        }),
    );
    assert_eq!(actions[0]["title"], "Download a JDK…");
    assert_eq!(
        actions[0]["command"]["command"],
        super::DOWNLOAD_JDK_COMMAND
    );

    // The command asks again, from the start.
    client.request(
        "workspace/executeCommand",
        json!({ "command": super::DOWNLOAD_JDK_COMMAND, "arguments": [] }),
    );
    let again = client.take_server_request("window/showMessageRequest");
    assert_eq!(button_titles(&again)[0], "Eclipse Temurin");
    client.reply(again, Value::Null);
    // Declined twice: nothing downloads.
    client.request("workspace/symbol", json!({ "query": "x" }));
    let downloading = client
        .unread
        .iter()
        .any(|notification| notification.params["token"] == "kotlin-analyzer/jdk-download");
    assert!(!downloading);
    client.shut_down();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn with_a_jdk_the_server_asks_nothing_and_offers_no_download() {
    let root = workspace("has-jdk");
    let java_home = root.join("jdk");
    std::fs::create_dir_all(java_home.join("bin")).unwrap();
    std::fs::write(java_home.join("bin/java"), "").unwrap();
    let java = project_model::jdk::JavaEnvironment {
        java_home: Some(java_home),
        ..no_jdk(&root)
    };
    let (mut client, _) =
        Client::start_with_java(&root, json!({ "gradleExport": false }), json!({}), java);
    client.wait_for_load(1);
    let file = root.join("Main.kt");
    client.open(&file, "fun main() {}\n");
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": { "uri": uri(&file) },
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            "context": { "diagnostics": [] },
        }),
    );
    // No download offered: only the source action every file has.
    let titles: Vec<&str> = actions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|action| action["title"].as_str())
        .collect();
    assert_eq!(titles, ["Invalidate caches and restart kotlin-analyzer"]);
    assert!(
        !client
            .server_requests
            .iter()
            .any(|method| method == "window/showMessageRequest"),
        "{:?}",
        client.server_requests
    );
    client.shut_down();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn offer_jdk_download_false_asks_nothing_at_startup() {
    let root = workspace("no-jdk-quiet");
    let (mut client, _) = Client::start_with_java(
        &root,
        json!({ "gradleExport": false, "offerJdkDownload": false }),
        json!({}),
        no_jdk(&root),
    );
    client.wait_for_load(1);
    client.request("workspace/symbol", json!({ "query": "x" }));
    assert!(
        !client
            .server_requests
            .iter()
            .any(|method| method == "window/showMessageRequest"),
        "{:?}",
        client.server_requests
    );
    client.shut_down();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_download_command_names_a_vendor_and_a_release_or_asks() {
    use super::jdk_download::Vendor;
    assert_eq!(
        super::chosen_jdk(&[json!("temurin"), json!(21)]),
        Some((Vendor::Temurin, 21))
    );
    assert_eq!(
        super::chosen_jdk(&[json!("graalvm_community"), json!(25)]),
        Some((Vendor::GraalVmCommunity, 25))
    );
    assert_eq!(super::chosen_jdk(&[]), None);
    assert_eq!(
        super::chosen_jdk(&[json!("corretto"), json!(21)]),
        Some((Vendor::Corretto, 21))
    );
    assert_eq!(super::chosen_jdk(&[json!("sap_machine"), json!(21)]), None);
    assert_eq!(super::chosen_jdk(&[json!("temurin"), json!("21")]), None);
}

#[test]
fn hierarchy_lenses_show_what_a_member_overrides_and_what_implements_it() {
    let root = workspace("hierarchy-lenses");
    let sources = root.join("src/main/kotlin/app");
    let file = sources.join("Providers.kt");
    let text = "package app\n\nabstract class Provider {\n    abstract val id: String\n    open fun poll(): Int = 1\n}\n\nclass Bank : Provider() {\n    override val id = \"bank\"\n}\n\nclass Card : Provider() {\n    override val id = \"card\"\n}\n";
    std::fs::write(&file, text).unwrap();
    let (mut client, _) = Client::start_offering(
        &root,
        json!({
            "gradleExport": false,
            "codeLens": { "locationsCommand": "editor.action.showReferences" },
        }),
        json!({}),
    );
    client.wait_for_load(1);
    client.open(&file, text);
    let document = json!({ "textDocument": { "uri": uri(&file) } });
    let lenses = client.request("textDocument/codeLens", document);
    let resolved_at = |client: &mut Client, line: u32, found: &str| -> Value {
        let lens = lenses
            .as_array()
            .unwrap()
            .iter()
            .find(|lens| lens["range"]["start"]["line"] == line && lens["data"]["found"] == found)
            .unwrap_or_else(|| panic!("no {found} lens on line {line}: {lenses}"))
            .clone();
        client.request("codeLens/resolve", lens)["command"].clone()
    };
    // ↑ from Bank.id to Provider.id, with Zed's command.
    let overridden = resolved_at(&mut client, 8, "overridden");
    assert_eq!(overridden["title"], "↑ Provider.id");
    assert_eq!(overridden["command"], "editor.action.showReferences");
    assert_eq!(
        overridden["arguments"][2],
        json!([{ "uri": uri(&file), "range": { "start": { "line": 3, "character": 17 }, "end": { "line": 3, "character": 19 } } }])
    );
    // ↓ from Provider.id to both overrides, from the class to both subclasses.
    let implementations = resolved_at(&mut client, 3, "implementations");
    assert_eq!(implementations["title"], "↓ 2 implementations");
    assert_eq!(implementations["arguments"][2].as_array().unwrap().len(), 2);
    assert_eq!(
        resolved_at(&mut client, 2, "implementations")["title"],
        "↓ 2 implementations"
    );
    // An open member nothing overrides: said, with nothing to run.
    let none = resolved_at(&mut client, 4, "implementations");
    assert_eq!(
        none,
        json!({ "title": "no implementations", "command": "" })
    );
    // A reference count of nothing still shows the (no) references.
    let unreferenced = resolved_at(&mut client, 4, "references");
    assert_eq!(unreferenced["title"], "0 references");
    assert_eq!(unreferenced["command"], "editor.action.showReferences");
    assert_eq!(unreferenced["arguments"][2], json!([]));
    client.shut_down();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn after_an_edit_inside_a_body_only_the_edited_file_is_checked_again() {
    let root = workspace("pull-incremental");
    let sources = root.join("src/main/kotlin/app");
    let order = sources.join("Order.kt");
    let order_text =
        "package app\n\nclass Order {\n    fun total(): Int {\n        return 1\n    }\n}\n";
    let usage = sources.join("Use.kt");
    let usage_text = "package app\n\nfun use(order: Order): Int = order.total()\n";
    std::fs::write(&order, order_text).unwrap();
    std::fs::write(&usage, usage_text).unwrap();
    let (mut client, _) = Client::start_offering(
        &root,
        json!({ "gradleExport": false }),
        json!({ "textDocument": { "diagnostic": {} } }),
    );
    client.wait_for_load(1);
    client.open(&order, order_text);
    // Which files a workspace pull, given the ids from the last one,
    // reports in full; and the ids it gives.
    let pull = |client: &mut Client, previous: &Value| -> (Vec<String>, Value) {
        let answer = client.request(
            "workspace/diagnostic",
            json!({ "previousResultIds": previous }),
        );
        let items = answer["items"].as_array().unwrap().clone();
        let mut full: Vec<String> = items
            .iter()
            .filter(|item| item["kind"] == "full")
            .map(|item| {
                item["uri"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        full.sort();
        let ids: Vec<Value> = items
            .iter()
            .map(|item| json!({ "uri": item["uri"], "value": item["resultId"] }))
            .collect();
        (full, Value::Array(ids))
    };
    let edit = |client: &mut Client, version: i32, text: &str| {
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri(&order), "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
    };
    let (full, ids) = pull(&mut client, &json!([]));
    assert_eq!(full, ["Order.kt", "Use.kt"]);
    let (full, ids) = pull(&mut client, &ids);
    assert!(full.is_empty(), "nothing changed: {full:?}");

    // Whitespace and a body under a declared type: Order.kt alone.
    edit(
        &mut client,
        2,
        "package app\n\nclass Order {\n    fun total(): Int {\n        return  1 + 2\n    }\n}\n",
    );
    let (full, ids) = pull(&mut client, &ids);
    assert_eq!(full, ["Order.kt"]);

    // A signature: every file, Use.kt reading it.
    edit(
        &mut client,
        3,
        "package app\n\nclass Order {\n    fun total(): Long {\n        return 1\n    }\n}\n",
    );
    let (full, _) = pull(&mut client, &ids);
    assert_eq!(full, ["Order.kt", "Use.kt"]);
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn invalidating_the_caches_reads_the_project_again_keeping_what_is_open() {
    let root = workspace("invalidate");
    let sources = root.join("src/main/kotlin/app");
    let saved = sources.join("Saved.kt");
    std::fs::write(&saved, "package app\n\nclass Saved\n").unwrap();
    let open = sources.join("Open.kt");
    std::fs::write(&open, "package app\n\nclass OnDisk\n").unwrap();
    let mut client = Client::start(&root);
    client.wait_for_load(1);
    // Unsaved in the editor.
    client.open(&open, "package app\n\nclass Unsaved\n");
    // Gone from the disk without the client saying so: a load from scratch
    // doesn't find it.
    std::fs::remove_file(&saved).unwrap();
    client.request(
        "workspace/executeCommand",
        json!({ "command": super::INVALIDATE_COMMAND, "arguments": [] }),
    );
    let message = client.wait_for(|notification| notification.method == "window/showMessage");
    assert!(
        message.params["message"]
            .as_str()
            .unwrap()
            .contains("caches invalidated"),
        "{}",
        message.params
    );
    client.wait_for_load(2);
    assert_eq!(
        client.workspace_symbols("Unsaved"),
        ["Unsaved"],
        "the editor's text stays"
    );
    assert!(client.workspace_symbols("OnDisk").is_empty());
    let saved_classes = client.workspace_symbols("Saved");
    assert!(
        !saved_classes.iter().any(|name| name == "Saved"),
        "read again: gone, {saved_classes:?}"
    );
    client.shut_down();
    std::fs::remove_dir_all(&root).unwrap();
}
