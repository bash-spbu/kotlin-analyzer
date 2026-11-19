//! A host this process runs for a project no other host answers for: the
//! language server on a thread, driven over an in-memory connection the
//! way an editor would drive it, listening on the project's socket like
//! any host (other agents' MCP processes ask it too).
//!
//! No editor tells it which files changed, and an agent edits them: before
//! each query the project's sources are looked at again, and what changed
//! is reported (`workspace/didChangeWatchedFiles`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use serde_json::json;

/// What a file looked like when last seen: when it changed, its size.
type Seen = (SystemTime, u64);

pub(crate) struct OwnHost {
    connection: Connection,
    /// The server's thread: joined on the way out, so its socket file goes
    /// with it.
    server: Option<std::thread::JoinHandle<()>>,
    root: PathBuf,
    seen: HashMap<PathBuf, Seen>,
    next_id: i32,
}

impl OwnHost {
    /// Starts the server for `root`, listening on `sockets`; exporting
    /// the Gradle model in the background when `gradle_export`.
    pub(crate) fn start(
        root: &Path,
        sockets: &Path,
        gradle_export: bool,
    ) -> Result<OwnHost, String> {
        let (server, connection) = Connection::memory();
        let thread = std::thread::Builder::new()
            .name("kotlin-analyzer-host".to_owned())
            .spawn(move || {
                let _ = crate::server::run_on(&server);
            })
            .map_err(|error| error.to_string())?;
        let mut host = OwnHost {
            connection,
            server: Some(thread),
            root: root.to_path_buf(),
            seen: sources(root),
            next_id: 0,
        };
        let uri = crate::server::uri_of(root).ok_or("the project's path isn't a file URI")?;
        host.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri.as_str(),
                "capabilities": {},
                "initializationOptions": { "agentSockets": sockets, "gradleExport": gradle_export },
            }),
        )?;
        host.notify("initialized", json!({}));
        Ok(host)
    }

    /// Tells the server what changed in the project's sources since last
    /// time, and waits until it has taken it in.
    pub(crate) fn catch_up(&mut self) -> Result<(), String> {
        let now = sources(&self.root);
        let mut changes = Vec::new();
        for (path, seen) in &now {
            let change = match self.seen.get(path) {
                None => Some(1),
                Some(before) if before != seen => Some(2),
                Some(_) => None,
            };
            if let Some(kind) = change {
                changes.push((path.clone(), kind));
            }
        }
        for path in self.seen.keys().filter(|path| !now.contains_key(*path)) {
            changes.push((path.clone(), 3));
        }
        self.seen = now;
        if changes.is_empty() {
            return Ok(());
        }
        let events: Vec<serde_json::Value> = changes
            .iter()
            .filter_map(|(path, kind)| {
                let uri = crate::server::uri_of(path)?;
                Some(json!({ "uri": uri.as_str(), "type": kind }))
            })
            .collect();
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": events }),
        );
        // Answered after the notification is taken in: the connection keeps
        // their order.
        self.request("workspace/symbol", json!({ "query": "" }))?;
        Ok(())
    }

    /// Stops the server the way an editor does.
    fn stop(&mut self) {
        let _ = self.request("shutdown", serde_json::Value::Null);
        self.notify("exit", serde_json::Value::Null);
        if let Some(thread) = self.server.take() {
            let _ = thread.join();
        }
    }

    fn notify(&self, method: &str, params: serde_json::Value) {
        let notification = Notification::new(method.to_owned(), params);
        let _ = self
            .connection
            .sender
            .send(Message::Notification(notification));
    }

    /// Sends a request and waits for its answer, answering the server's
    /// own requests (progress, registrations, refreshes) meanwhile.
    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        let request = Request::new(id.clone(), method.to_owned(), params);
        self.connection
            .sender
            .send(Message::Request(request))
            .map_err(|_| "the host stopped".to_owned())?;
        loop {
            match self.connection.receiver.recv() {
                Ok(Message::Response(response)) if response.id == id => {
                    return response.response_result.map_err(|error| error.message);
                }
                Ok(Message::Request(request)) => {
                    let answer = Response::new_ok(request.id, serde_json::Value::Null);
                    let _ = self.connection.sender.send(Message::Response(answer));
                }
                Ok(Message::Response(_) | Message::Notification(_)) => {}
                Err(_) => return Err("the host stopped".to_owned()),
            }
        }
    }
}

impl Drop for OwnHost {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The project's sources and build files, as last changed: what the
/// server reads.
fn sources(root: &Path) -> HashMap<PathBuf, Seen> {
    let skipped = |name: &str| {
        name.starts_with('.') || matches!(name, "build" | "out" | "target" | "node_modules")
    };
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            let is_root = entry.depth() == 0;
            is_root
                || !(entry.file_type().is_dir() && skipped(&entry.file_name().to_string_lossy()))
        })
        .flatten()
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy();
            [".kt", ".kts", ".java", ".gradle"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            Some((
                entry.into_path(),
                (metadata.modified().ok()?, metadata.len()),
            ))
        })
        .collect()
}
