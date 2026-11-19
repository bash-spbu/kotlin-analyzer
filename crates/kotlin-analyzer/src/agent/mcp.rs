//! `kotlin-analyzer mcp`: an MCP server over stdio (JSON-RPC, a message a
//! line) whose tools answer from the host of the file's project: the
//! language server an editor runs for it, found by its socket, else one
//! this process starts (`host`). A git worktree is a project of its own,
//! so agents in parallel worktrees each get theirs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::host::OwnHost;
use super::protocol::{self, AgentQuery, AgentReply, AgentRequest, SourcePosition};

/// How long a tool waits for its answer: a project still loading asks the
/// agent to come back rather than outwait the client's own timeout.
const ANSWER_PATIENCE: Duration = Duration::from_secs(45);
/// How long a host this process starts gets to open its socket.
const START_PATIENCE: Duration = Duration::from_secs(10);

const INSTRUCTIONS: &str = "Semantic navigation of Kotlin and Java code \
(Gradle projects), from the same analysis the editor uses: where a name is \
declared, where it is used, its type and documentation, what implements it, \
a file's outline and problems. Prefer these to text search for Kotlin \
symbols: they resolve imports, overloads and inheritance. Positions are a \
file, a line (from 1) and the symbol's name on that line.";

pub(crate) fn command(arguments: &[String]) -> ExitCode {
    let mut sockets = protocol::default_socket_directory();
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        if argument == "--socket-directory" {
            sockets = remaining.next().map(PathBuf::from);
        }
    }
    let Some(sockets) = sockets else {
        eprintln!(
            "kotlin-analyzer mcp: no cache directory for the sockets (set HOME or XDG_CACHE_HOME)"
        );
        return ExitCode::FAILURE;
    };
    crate::log::enable_output();
    let working_directory = std::env::current_dir().unwrap_or_default();
    let mut server = McpServer::new(Hosts::new(sockets), working_directory);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle(&line) {
            let _ = writeln!(stdout, "{response}");
            let _ = stdout.flush();
        }
    }
    ExitCode::SUCCESS
}

/// Who answers queries for a project.
pub(crate) trait Answerer {
    fn ask(&mut self, root: &Path, query: AgentQuery) -> Result<String, String>;
}

pub(crate) struct McpServer<A> {
    answerer: A,
    /// The agent's debug session, if any.
    debug: super::debug::DebugTools,
    /// Where a query without a file looks: the directory this process
    /// started in.
    working_directory: PathBuf,
}

impl<A: Answerer> McpServer<A> {
    pub(crate) fn new(answerer: A, working_directory: PathBuf) -> McpServer<A> {
        McpServer {
            answerer,
            debug: super::debug::DebugTools::default(),
            working_directory,
        }
    }

    /// The response to one JSON-RPC message; none for a notification.
    pub(crate) fn handle(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                return Some(error_response(
                    &Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
            }
        };
        let id = message.get("id")?.clone();
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "kotlin-analyzer", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let mut listed = tools().as_array().cloned().unwrap_or_default();
                listed.extend(super::debug::DebugTools::tools());
                Ok(json!({ "tools": listed }))
            }
            "tools/call" => Ok(self.call(params)),
            _ => Err((-32601, format!("unknown method {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
            Err((code, message)) => error_response(&id, code, &message),
        })
    }

    /// A tool's result: its answer as text, or why there is none.
    fn call(&mut self, params: &Value) -> Value {
        let name = params["name"].as_str().unwrap_or_default();
        let arguments = &params["arguments"];
        let answered = if super::debug::DebugTools::handles(name) {
            self.debug.call(name, arguments, &self.working_directory)
        } else {
            query_of(name, arguments, &self.working_directory)
                .and_then(|(root, query)| self.answerer.ask(&root, query))
        };
        let (text, is_error) = match answered {
            Ok(text) => (text, false),
            Err(why) => (why, true),
        };
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
    }
}

fn error_response(id: &Value, code: i32, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// The tools, as `tools/list` describes them.
fn tools() -> Value {
    let position = |what: &str| {
        json!({
            "type": "object",
            "properties": {
                "file": { "type": "string", "description": "The Kotlin or Java file: absolute, or relative to the working directory." },
                "line": { "type": "integer", "minimum": 1, "description": "The line the name is on, from 1." },
                "symbol": { "type": "string", "description": format!("The name on that line {what}.") },
                "column": { "type": "integer", "minimum": 1, "description": "Instead of `symbol`: the column, from 1, in characters." },
            },
            "required": ["file", "line"],
        })
    };
    let file_only = json!({
        "type": "object",
        "properties": {
            "file": { "type": "string", "description": "The Kotlin or Java file: absolute, or relative to the working directory." },
        },
        "required": ["file"],
    });
    json!([
        {
            "name": "symbols",
            "description": "Find declarations (classes, functions, properties) by name across the project: exact, prefix, camel-case humps (`ODS` finds `OrderDetailService`) or fuzzy. Answers each with file:line:column, kind and container.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The name, or part of it." },
                    "root": { "type": "string", "description": "A directory in the project (default: the working directory)." },
                },
                "required": ["query"],
            },
        },
        {
            "name": "definition",
            "description": "Where the name at a position is declared: the class, function, property, parameter or local it resolves to (into library sources too), with file:line:column and the declaration's line.",
            "inputSchema": position("to look up"),
        },
        {
            "name": "references",
            "description": "Every use of the declaration (or local) named at a position, across the project, resolved — a same-named declaration elsewhere isn't counted. Answers file:line:column and the line of each.",
            "inputSchema": position("whose uses to find"),
        },
        {
            "name": "hover",
            "description": "The signature, inferred type and documentation of the name at a position, as markdown.",
            "inputSchema": position("to describe"),
        },
        {
            "name": "implementations",
            "description": "The subclasses of a class or interface named at a position, or the overrides of a function or property, across the project.",
            "inputSchema": position("whose implementations to find"),
        },
        {
            "name": "outline",
            "description": "A file's declarations as a tree: kind, name, signature and line of each.",
            "inputSchema": file_only.clone(),
        },
        {
            "name": "diagnostics",
            "description": "The problems kotlin-analyzer's checks find in a file (unresolved names, missing imports, wrong arguments, nullability, …): line:column, severity, check and message. It reports only what it is sure of; it is not the compiler.",
            "inputSchema": file_only,
        },
    ])
}

/// The project root and the query a tool call asks.
fn query_of(
    name: &str,
    arguments: &Value,
    working_directory: &Path,
) -> Result<(PathBuf, AgentQuery), String> {
    let path_of = |key: &str| -> Result<PathBuf, String> {
        let written = arguments[key]
            .as_str()
            .ok_or_else(|| format!("`{key}` is required"))?;
        let path = PathBuf::from(written);
        Ok(if path.is_absolute() {
            path
        } else {
            working_directory.join(path)
        })
    };
    let position = || -> Result<SourcePosition, String> {
        let line = arguments["line"].as_u64().ok_or("`line` is required")?;
        Ok(SourcePosition {
            file: path_of("file")?,
            line: u32::try_from(line).unwrap_or(u32::MAX),
            symbol: arguments["symbol"].as_str().map(str::to_owned),
            column: arguments["column"]
                .as_u64()
                .map(|column| u32::try_from(column).unwrap_or(u32::MAX)),
        })
    };
    let query = match name {
        "symbols" => {
            let query = arguments["query"]
                .as_str()
                .ok_or("`query` is required")?
                .to_owned();
            let root = match arguments["root"].as_str() {
                Some(_) => path_of("root")?,
                None => working_directory.to_path_buf(),
            };
            let project = protocol::project_root(&root).ok_or_else(|| {
                format!("{} isn't in a Gradle project; pass `root`", root.display())
            })?;
            return Ok((project, AgentQuery::Symbols { query }));
        }
        "definition" => AgentQuery::Definition { at: position()? },
        "references" => AgentQuery::References { at: position()? },
        "hover" => AgentQuery::Hover { at: position()? },
        "implementations" => AgentQuery::Implementations { at: position()? },
        "outline" => AgentQuery::Outline {
            file: path_of("file")?,
        },
        "diagnostics" => AgentQuery::Diagnostics {
            file: path_of("file")?,
        },
        unknown => return Err(format!("no tool `{unknown}`")),
    };
    let file = query.file().map(Path::to_path_buf).unwrap_or_default();
    if !file.is_file() {
        return Err(format!("{} doesn't exist", file.display()));
    }
    let project = protocol::project_root(&file)
        .ok_or_else(|| format!("{} isn't in a Gradle project", file.display()))?;
    Ok((project, query))
}

/// The hosts this process asks, by project root: connections to their
/// sockets, and the hosts it started itself.
pub(crate) struct Hosts {
    sockets: PathBuf,
    /// Whether a host this process starts exports the Gradle model.
    gradle_export: bool,
    connections: HashMap<PathBuf, HostConnection>,
    own: HashMap<PathBuf, OwnHost>,
    next_id: u64,
}

struct HostConnection {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Hosts {
    pub(crate) fn new(sockets: PathBuf) -> Hosts {
        Hosts {
            sockets,
            gradle_export: true,
            connections: HashMap::new(),
            own: HashMap::new(),
            next_id: 0,
        }
    }

    /// A connection to `root`'s host: the one there is, else one this
    /// process starts.
    fn connect(&mut self, root: &Path) -> Result<HostConnection, String> {
        let path = protocol::socket_path(&self.sockets, root);
        if path.as_os_str().len() > protocol::SOCKET_PATH_LIMIT {
            return Err(format!(
                "the socket directory {} is too deep for a Unix socket; pass a shorter --socket-directory",
                self.sockets.display()
            ));
        }
        if let Ok(stream) = UnixStream::connect(&path) {
            return HostConnection::of(stream);
        }
        if !self.own.contains_key(root) {
            let host = OwnHost::start(root, &self.sockets, self.gradle_export)?;
            self.own.insert(root.to_path_buf(), host);
        }
        let started = Instant::now();
        loop {
            if let Ok(stream) = UnixStream::connect(&path) {
                return HostConnection::of(stream);
            }
            if started.elapsed() > START_PATIENCE {
                return Err(format!(
                    "kotlin-analyzer couldn't start for {}",
                    root.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn ask_once(
        &mut self,
        root: &Path,
        query: &AgentQuery,
    ) -> Result<Result<String, String>, std::io::Error> {
        if !self.connections.contains_key(root) {
            let connection = self.connect(root).map_err(std::io::Error::other)?;
            self.connections.insert(root.to_path_buf(), connection);
        }
        if let Some(host) = self.own.get_mut(root) {
            host.catch_up().map_err(std::io::Error::other)?;
        }
        self.next_id += 1;
        let id = self.next_id;
        let connection = self.connections.get_mut(root).expect("connected above");
        protocol::write_line(
            &mut connection.writer,
            &AgentRequest {
                id,
                query: query.clone(),
            },
        )?;
        let deadline = Instant::now() + ANSWER_PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Err(
                    "The project is still loading; ask again in a minute.".to_owned()
                ));
            }
            connection.reader.get_ref().set_read_timeout(Some(left))?;
            match protocol::read_line::<AgentReply>(&mut connection.reader) {
                // A reply to a query given up on earlier: not this one.
                Ok(Some(reply)) if reply.id != id => continue,
                Ok(Some(reply)) => {
                    return Ok(match (reply.answer, reply.error) {
                        (Some(answer), _) => Ok(answer),
                        (None, error) => Err(error.unwrap_or_default()),
                    });
                }
                Ok(None) => return Err(std::io::Error::other("the host closed the connection")),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(Err(
                        "The project is still loading; ask again in a minute.".to_owned()
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl HostConnection {
    fn of(stream: UnixStream) -> Result<HostConnection, String> {
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        Ok(HostConnection {
            reader: BufReader::new(stream),
            writer,
        })
    }
}

impl Answerer for Hosts {
    /// Asks `root`'s host; a host gone (its editor closed) is replaced
    /// once — by another, or by one this process starts.
    fn ask(&mut self, root: &Path, query: AgentQuery) -> Result<String, String> {
        match self.ask_once(root, &query) {
            Ok(answered) => answered,
            Err(_) => {
                self.connections.remove(root);
                self.ask_once(root, &query).unwrap_or_else(|error| {
                    Err(format!("kotlin-analyzer isn't answering: {error}"))
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Gradle-shaped project at `root` with an order and its use.
    fn project(root: &Path) {
        let _ = std::fs::remove_dir_all(root);
        let sources = root.join("src/main/kotlin/shop");
        std::fs::create_dir_all(&sources).unwrap();
        std::fs::write(root.join("settings.gradle.kts"), "").unwrap();
        std::fs::write(
            sources.join("Order.kt"),
            "package shop\n\n/** An order of [items]. */\nclass Order(val items: List<String>) {\n    fun total(): Int = items.size\n}\n",
        )
        .unwrap();
        std::fs::write(
            sources.join("Checkout.kt"),
            "package shop\n\nfun checkout(order: Order): Int = order.total() + order.total()\n",
        )
        .unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        // Short: a Unix socket's path has a length limit.
        std::env::temp_dir().join(format!("ka-{name}-{}", std::process::id()))
    }

    fn call(
        server: &mut McpServer<Hosts>,
        id: u32,
        tool: &str,
        arguments: Value,
    ) -> (String, bool) {
        let request = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        let response: Value =
            serde_json::from_str(&server.handle(&request.to_string()).unwrap()).unwrap();
        let result = &response["result"];
        (
            result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            result["isError"].as_bool().unwrap_or(true),
        )
    }

    fn hosts(sockets: &Path) -> Hosts {
        let mut hosts = Hosts::new(sockets.to_path_buf());
        hosts.gradle_export = false;
        hosts
    }

    #[test]
    fn the_server_lists_its_tools_and_answers_from_a_host_it_starts() {
        let root = scratch("mcp");
        let sockets = scratch("mcp-sockets");
        project(&root);
        let mut server = McpServer::new(hosts(&sockets), root.clone());
        let initialized: Value = serde_json::from_str(
            &server
                .handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            initialized["result"]["serverInfo"]["name"],
            "kotlin-analyzer"
        );
        assert_eq!(
            server.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        let listed: Value = serde_json::from_str(
            &server
                .handle(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
                .unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names[..7],
            [
                "symbols",
                "definition",
                "references",
                "hover",
                "implementations",
                "outline",
                "diagnostics"
            ]
        );
        assert!(
            names.contains(&"debug_start") && names.contains(&"debug_evaluate"),
            "{names:?}"
        );

        let checkout = "src/main/kotlin/shop/Checkout.kt";
        let (definition, failed) = call(
            &mut server,
            3,
            "definition",
            json!({ "file": checkout, "line": 3, "symbol": "total" }),
        );
        assert!(!failed, "{definition}");
        assert_eq!(
            definition,
            "src/main/kotlin/shop/Order.kt:5:9 — method total in Order\n    fun total(): Int = items.size"
        );
        let (references, _) = call(
            &mut server,
            4,
            "references",
            json!({ "file": checkout, "line": 3, "symbol": "total" }),
        );
        assert!(references.starts_with("2 references\n"), "{references}");
        let (hover, _) = call(
            &mut server,
            5,
            "hover",
            json!({ "file": checkout, "line": 3, "symbol": "Order" }),
        );
        assert!(
            hover.contains("class Order(items: List<String>)")
                && hover.contains("An order of `items`."),
            "{hover}"
        );
        let (symbols, _) = call(&mut server, 6, "symbols", json!({ "query": "checkout" }));
        assert!(
            symbols.starts_with("src/main/kotlin/shop/Checkout.kt:3:5 — function checkout in shop"),
            "{symbols}"
        );
        let (outline, _) = call(
            &mut server,
            7,
            "outline",
            json!({ "file": "src/main/kotlin/shop/Order.kt" }),
        );
        assert!(
            outline.contains("class Order") && outline.contains("  method total(): Int (line 5)"),
            "{outline}"
        );
        let (missing, failed) = call(
            &mut server,
            8,
            "definition",
            json!({ "file": checkout, "line": 3, "symbol": "price" }),
        );
        assert!(
            failed && missing.contains("`price` isn't on line 3"),
            "{missing}"
        );

        // An agent's edit on disk is seen at the next query.
        std::fs::write(
            root.join(checkout),
            "package shop\n\nfun checkout(order: Order): Int = order.total()\n",
        )
        .unwrap();
        let (references, _) = call(
            &mut server,
            9,
            "references",
            json!({ "file": "src/main/kotlin/shop/Order.kt", "line": 5, "symbol": "total" }),
        );
        assert!(references.starts_with("1 references\n"), "{references}");

        // Another agent's process asks the same host instead of starting one.
        let mut other = McpServer::new(hosts(&sockets), root.clone());
        let (definition, failed) = call(
            &mut other,
            1,
            "definition",
            json!({ "file": checkout, "line": 3, "symbol": "total" }),
        );
        assert!(!failed, "{definition}");
        assert!(
            other.answerer.own.is_empty(),
            "no second host for the same project"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_worktree_gets_its_own_host_beside_the_projects() {
        let root = scratch("wt");
        let sockets = scratch("wt-sockets");
        project(&root);
        let worktree = root.join(".worktrees/fix");
        project(&worktree);
        std::fs::write(
            worktree.join("src/main/kotlin/shop/Checkout.kt"),
            "package shop\n\nfun checkout(order: Order): Int =\n    order.total()\n",
        )
        .unwrap();
        let mut server = McpServer::new(hosts(&sockets), root.clone());
        let in_project = call(
            &mut server,
            1,
            "references",
            json!({ "file": "src/main/kotlin/shop/Order.kt", "line": 5, "symbol": "total" }),
        );
        let in_worktree = call(
            &mut server,
            2,
            "references",
            json!({ "file": worktree.join("src/main/kotlin/shop/Order.kt"), "line": 5, "symbol": "total" }),
        );
        assert!(in_project.0.starts_with("2 references"), "{}", in_project.0);
        assert!(
            in_worktree
                .0
                .starts_with("1 references\nsrc/main/kotlin/shop/Checkout.kt:4:11"),
            "{}",
            in_worktree.0
        );
        assert_eq!(server.answerer.own.len(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
