//! `kotlin-analyzer dap`: a Debug Adapter Protocol server over stdio for
//! editors (Zed's debugger), on the `debugger` crate.
//!
//! The configuration (`launch` / `attach` arguments):
//!
//! ```json
//! { "request": "launch", "test": { "file": "…/OrderTest.kt", "package": "shop",
//!   "outerClass": "", "class": "OrderTest", "method": "`creates an order`" } }
//! { "request": "launch", "program": "/build/gradlew", "args": ["-p", "app", "bootRun"], "cwd": "/build" }
//! { "request": "launch", "program": "java", "args": ["-cp", "classes", "shop.MainKt"], "cwd": "/build" }
//! { "request": "attach", "host": "localhost", "port": 5005 }
//! ```
//!
//! A test runs through Gradle's test task in the file's module; a Gradle
//! program's `Test`/`JavaExec` JVMs get the debug agent (see
//! `debugger::launch`). The flow is DAP's: `initialize`, then `launch`
//! answered once the JVM has connected (the program's output streams to the
//! console meanwhile), `initialized`, breakpoints, `configurationDone` —
//! and the JVM runs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use debugger::{Launch, Session, Stop};
use jdwp::{ObjectId, StepDepth, ThreadId};
use serde_json::{Value, json};

pub(crate) fn command() -> ExitCode {
    let (requests, incoming) = std::sync::mpsc::channel();
    std::thread::spawn(move || read_messages(std::io::stdin().lock(), &requests));
    let mut adapter = Adapter::new(Box::new(std::io::stdout()));
    adapter.run(&incoming);
    ExitCode::SUCCESS
}

/// Reads DAP messages (`Content-Length` framed JSON) until the input ends.
fn read_messages(input: impl Read, messages: &Sender<Value>) {
    let mut reader = BufReader::new(input);
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length:") {
                length = value.trim().parse::<usize>().ok();
            }
        }
        let Some(length) = length else { continue };
        let mut body = vec![0u8; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        if let Ok(message) = serde_json::from_slice(&body)
            && messages.send(message).is_err()
        {
            return;
        }
    }
}

/// What a `variablesReference` names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reference {
    /// A frame's variables.
    Frame { thread: ThreadId, index: usize },
    /// An object's fields or elements.
    Object(ObjectId),
}

/// A session starting on another thread: the JVM's output meanwhile, and
/// the session (or why not) at the end.
struct Starting {
    request: Value,
    started: Receiver<Result<Session, String>>,
}

pub(crate) struct Adapter {
    output: Box<dyn Write + Send>,
    sequence: i64,
    session: Option<Session>,
    starting: Option<Starting>,
    lines: Option<Receiver<String>>,
    root: PathBuf,
    running: bool,
    ended: bool,
    /// DAP's ids are small numbers: index + 1 in these.
    threads: Vec<ThreadId>,
    frames: Vec<(ThreadId, usize)>,
    references: Vec<Reference>,
    /// The debugger's breakpoints of each file, to replace them all as
    /// `setBreakpoints` does.
    breakpoints: HashMap<PathBuf, Vec<u32>>,
}

impl Adapter {
    pub(crate) fn new(output: Box<dyn Write + Send>) -> Adapter {
        Adapter {
            output,
            sequence: 0,
            session: None,
            starting: None,
            lines: None,
            root: std::env::current_dir().unwrap_or_default(),
            running: false,
            ended: false,
            threads: Vec::new(),
            frames: Vec::new(),
            references: Vec::new(),
            breakpoints: HashMap::new(),
        }
    }

    pub(crate) fn run(&mut self, incoming: &Receiver<Value>) {
        loop {
            self.forward_output();
            self.check_started();
            if self.running {
                let stop = self
                    .session
                    .as_mut()
                    .and_then(|session| session.wait_for_stop(Duration::from_millis(20)));
                if let Some(stop) = stop {
                    self.stopped(&stop);
                }
            }
            match incoming.recv_timeout(Duration::from_millis(20)) {
                Ok(message) => {
                    if message["type"] == "request" && !self.handle(&message) {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(session) = self.session.take() {
                        session.terminate();
                    }
                    return;
                }
            }
        }
    }

    fn send(&mut self, mut message: Value) {
        self.sequence += 1;
        message["seq"] = json!(self.sequence);
        let body = message.to_string();
        let _ = write!(self.output, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = self.output.flush();
    }

    fn respond(&mut self, request: &Value, body: Result<Value, String>) {
        let mut response = json!({
            "type": "response",
            "request_seq": request["seq"],
            "command": request["command"],
            "success": body.is_ok(),
        });
        match body {
            Ok(body) => response["body"] = body,
            Err(message) => {
                response["message"] = json!(message);
                response["body"] =
                    json!({ "error": { "id": 1, "format": message, "showUser": true } });
            }
        }
        self.send(response);
    }

    fn event(&mut self, event: &str, body: Value) {
        self.send(json!({ "type": "event", "event": event, "body": body }));
    }

    fn forward_output(&mut self) {
        let lines: Vec<String> = match &self.lines {
            Some(lines) => lines.try_iter().collect(),
            None => return,
        };
        for line in lines {
            self.event(
                "output",
                json!({ "category": "stdout", "output": format!("{line}\n") }),
            );
        }
    }

    /// Answers `launch`/`attach` once the JVM has connected.
    fn check_started(&mut self) {
        let Some(starting) = &self.starting else {
            return;
        };
        let Ok(started) = starting.started.try_recv() else {
            return;
        };
        let starting = self.starting.take().expect("checked above");
        self.forward_output();
        match started {
            Ok(session) => {
                self.session = Some(session);
                self.respond(&starting.request, Ok(Value::Null));
                self.event("initialized", json!({}));
            }
            Err(why) => {
                self.respond(&starting.request, Err(why));
                self.event("terminated", json!({}));
            }
        }
    }

    /// Handles a request; `false` once the session is over.
    fn handle(&mut self, request: &Value) -> bool {
        let command = request["command"].as_str().unwrap_or_default();
        let arguments = &request["arguments"];
        let body = match command {
            "initialize" => Ok(json!({
                "supportsConfigurationDoneRequest": true,
                "supportsEvaluateForHovers": true,
                "supportsTerminateRequest": true,
                "exceptionBreakpointFilters": [
                    { "filter": "uncaught", "label": "Uncaught exceptions", "default": true },
                    { "filter": "all", "label": "All exceptions", "default": false },
                ],
            })),
            "launch" | "attach" => match launch_of(arguments, &self.root) {
                Ok((launch, root)) => {
                    self.root = root.clone();
                    let (line_sender, lines) = std::sync::mpsc::channel();
                    self.lines = Some(lines);
                    let (started_sender, started) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let started = Session::start_streaming(&launch, &[root], Some(line_sender));
                        let _ = started_sender.send(started);
                    });
                    self.starting = Some(Starting {
                        request: request.clone(),
                        started,
                    });
                    return true;
                }
                Err(why) => Err(why),
            },
            "setBreakpoints" => self.set_breakpoints(arguments),
            "setExceptionBreakpoints" => {
                let filters: Vec<&str> = arguments["filters"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let all = filters.contains(&"all");
                match self.session.as_mut() {
                    Some(session) => session
                        .break_on_exceptions(all, all || filters.contains(&"uncaught"))
                        .map(|()| Value::Null),
                    None => Ok(Value::Null),
                }
            }
            "configurationDone" => self.resume(),
            "threads" => self.threads(),
            "stackTrace" => self.stack_trace(arguments),
            "scopes" => {
                let frame = arguments["frameId"].as_u64().unwrap_or(0) as usize;
                match self.frames.get(frame.wrapping_sub(1)).copied() {
                    Some((thread, index)) => {
                        let reference = self.reference(Reference::Frame { thread, index });
                        Ok(
                            json!({ "scopes": [{ "name": "Locals", "variablesReference": reference, "expensive": false }] }),
                        )
                    }
                    None => Err("no such frame".to_owned()),
                }
            }
            "variables" => self.variables(arguments),
            "evaluate" => self.evaluate(arguments),
            "continue" => self
                .resume()
                .map(|_| json!({ "allThreadsContinued": true })),
            "next" | "stepIn" | "stepOut" => {
                let depth = match command {
                    "next" => StepDepth::Over,
                    "stepIn" => StepDepth::Into,
                    _ => StepDepth::Out,
                };
                self.step(arguments["threadId"].as_u64(), depth)
            }
            "pause" => match self.session.as_mut() {
                Some(session) => match session.pause() {
                    Ok(stop) => {
                        self.respond(request, Ok(Value::Null));
                        self.stopped(&stop);
                        return true;
                    }
                    Err(why) => Err(why),
                },
                None => Err("nothing to pause".to_owned()),
            },
            "disconnect" | "terminate" => {
                if let Some(session) = self.session.take() {
                    session.terminate();
                }
                self.respond(request, Ok(Value::Null));
                self.event("terminated", json!({}));
                return command == "terminate";
            }
            unknown => Err(format!("`{unknown}` isn't supported")),
        };
        self.respond(request, body);
        true
    }

    fn step(&mut self, thread_id: Option<u64>, depth: StepDepth) -> Result<Value, String> {
        let thread = self.thread_of(thread_id).ok_or("no such thread")?;
        self.forget_references();
        let session = self.session.as_mut().ok_or("no session")?;
        session.begin_step(thread, depth)?;
        self.running = true;
        Ok(Value::Null)
    }

    fn resume(&mut self) -> Result<Value, String> {
        let session = self.session.as_mut().ok_or("no session")?;
        if self.ended {
            return Ok(Value::Null);
        }
        session.resume()?;
        self.running = true;
        self.forget_references();
        Ok(Value::Null)
    }

    /// Ids of frames and variables are good until the program runs again.
    fn forget_references(&mut self) {
        self.frames.clear();
        self.references.clear();
    }

    fn reference(&mut self, reference: Reference) -> usize {
        self.references.push(reference);
        self.references.len()
    }

    fn thread_id(&mut self, thread: ThreadId) -> usize {
        match self.threads.iter().position(|known| *known == thread) {
            Some(index) => index + 1,
            None => {
                self.threads.push(thread);
                self.threads.len()
            }
        }
    }

    fn thread_of(&self, id: Option<u64>) -> Option<ThreadId> {
        let index = usize::try_from(id?).ok()?.checked_sub(1)?;
        self.threads.get(index).copied()
    }

    /// Tells the editor the program stopped (or ended).
    fn stopped(&mut self, stop: &Stop) {
        self.running = false;
        self.forget_references();
        self.forward_output();
        let (reason, description, breakpoints) = match stop {
            Stop::Breakpoint { breakpoints, .. } => ("breakpoint", None, breakpoints.clone()),
            Stop::Step { .. } => ("step", None, Vec::new()),
            Stop::Exception { exception, .. } => ("exception", Some(exception.clone()), Vec::new()),
            Stop::Paused { .. } => ("pause", None, Vec::new()),
            Stop::Ended { exit_code } => {
                self.ended = true;
                self.event("exited", json!({ "exitCode": exit_code.unwrap_or(0) }));
                self.event("terminated", json!({}));
                return;
            }
        };
        let Some(thread) = stop.thread() else { return };
        let thread_id = self.thread_id(thread);
        let mut body = json!({
            "reason": reason,
            "threadId": thread_id,
            "allThreadsStopped": true,
            "hitBreakpointIds": breakpoints,
        });
        if let Some(description) = description {
            body["description"] = json!(description);
            body["text"] = json!(description);
        }
        self.event("stopped", body);
    }

    fn set_breakpoints(&mut self, arguments: &Value) -> Result<Value, String> {
        let path = PathBuf::from(
            arguments["source"]["path"]
                .as_str()
                .ok_or("no source path")?,
        );
        let lines: Vec<u32> = arguments["breakpoints"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|breakpoint| breakpoint["line"].as_u64())
            .filter_map(|line| u32::try_from(line).ok())
            .collect();
        let session = self.session.as_mut().ok_or("no session")?;
        for id in self.breakpoints.remove(&path).unwrap_or_default() {
            session.remove_breakpoint(id);
        }
        let mut answered = Vec::new();
        let mut ids = Vec::new();
        for line in lines {
            match session.set_breakpoint(&path, line) {
                Ok(breakpoint) => {
                    ids.push(breakpoint.id);
                    answered.push(json!({
                        "id": breakpoint.id,
                        "verified": breakpoint.verified,
                        "line": line,
                        "source": { "path": path },
                    }));
                }
                Err(why) => {
                    answered.push(json!({ "verified": false, "line": line, "message": why }))
                }
            }
        }
        self.breakpoints.insert(path, ids);
        Ok(json!({ "breakpoints": answered }))
    }

    fn threads(&mut self) -> Result<Value, String> {
        let threads = match self.session.as_ref() {
            Some(session) if !self.ended => session.threads()?,
            _ => Vec::new(),
        };
        let listed: Vec<Value> = threads
            .into_iter()
            .map(|(thread, name)| json!({ "id": self.thread_id(thread), "name": name }))
            .collect();
        Ok(json!({ "threads": listed }))
    }

    fn stack_trace(&mut self, arguments: &Value) -> Result<Value, String> {
        let thread = self
            .thread_of(arguments["threadId"].as_u64())
            .ok_or("no such thread")?;
        let session = self.session.as_mut().ok_or("no session")?;
        let frames = session.stack(thread)?;
        let start = arguments["startFrame"].as_u64().unwrap_or(0) as usize;
        let levels = arguments["levels"]
            .as_u64()
            .filter(|levels| *levels > 0)
            .map(|levels| levels as usize);
        let shown: Vec<&debugger::Frame> = frames
            .iter()
            .skip(start)
            .take(levels.unwrap_or(usize::MAX))
            .collect();
        let mut stack_frames = Vec::with_capacity(shown.len());
        for frame in shown {
            self.frames.push((thread, frame.index));
            let mut entry = json!({
                "id": self.frames.len(),
                "name": format!("{} ({})", frame.method, frame.class_name.rsplit('.').next().unwrap_or("")),
                "line": frame.line.unwrap_or(0).max(0),
                "column": 1,
            });
            match (&frame.path, &frame.source_name) {
                (Some(path), _) => {
                    entry["source"] = json!({ "path": path, "name": path.file_name().map(|name| name.to_string_lossy().into_owned()) });
                }
                (None, Some(name)) => {
                    // A library's: no file to open; shown dimmed.
                    entry["source"] = json!({ "name": name, "presentationHint": "deemphasize" });
                    entry["presentationHint"] = json!("subtle");
                }
                (None, None) => entry["presentationHint"] = json!("subtle"),
            }
            stack_frames.push(entry);
        }
        Ok(json!({ "stackFrames": stack_frames, "totalFrames": frames.len() }))
    }

    fn variables(&mut self, arguments: &Value) -> Result<Value, String> {
        let index = arguments["variablesReference"].as_u64().unwrap_or(0) as usize;
        let reference = *self
            .references
            .get(index.wrapping_sub(1))
            .ok_or("no such reference")?;
        let session = self.session.as_mut().ok_or("no session")?;
        let shown = match reference {
            Reference::Frame { thread, index } => session.variables(thread, index)?,
            Reference::Object(object) => session.children(object)?,
        };
        let variables: Vec<Value> = shown
            .iter()
            .map(|variable| self.variable(variable))
            .collect();
        Ok(json!({ "variables": variables }))
    }

    fn variable(&mut self, variable: &debugger::Shown) -> Value {
        let reference = variable
            .children
            .map_or(0, |object| self.reference(Reference::Object(object)));
        json!({
            "name": variable.name,
            "value": variable.value,
            "type": variable.type_name,
            "variablesReference": reference,
        })
    }

    fn evaluate(&mut self, arguments: &Value) -> Result<Value, String> {
        let expression = arguments["expression"].as_str().ok_or("no expression")?;
        let frame = arguments["frameId"]
            .as_u64()
            .and_then(|id| self.frames.get((id as usize).wrapping_sub(1)).copied());
        let session = self.session.as_mut().ok_or("no session")?;
        let (thread, index) = match frame {
            Some(frame) => frame,
            None => (session.current_thread().ok_or("not stopped")?, 0),
        };
        let shown = session.evaluate(thread, index, expression)?;
        let reference = shown
            .children
            .map_or(0, |object| self.reference(Reference::Object(object)));
        Ok(
            json!({ "result": shown.value, "type": shown.type_name, "variablesReference": reference }),
        )
    }
}

/// The launch a configuration asks for, and the project's root.
fn launch_of(arguments: &Value, working_directory: &Path) -> Result<(Launch, PathBuf), String> {
    let request = arguments["request"].as_str().unwrap_or("launch");
    if request == "attach" {
        let host = arguments["host"].as_str().unwrap_or("localhost");
        let port = arguments["port"]
            .as_u64()
            .ok_or("`port` is required to attach")?;
        let root = arguments["cwd"]
            .as_str()
            .map_or_else(|| working_directory.to_path_buf(), PathBuf::from);
        return Ok((
            Launch::Attach {
                address: format!("{host}:{port}"),
            },
            root,
        ));
    }
    if let Some(test) = arguments.get("test").filter(|test| test.is_object()) {
        let file = PathBuf::from(test["file"].as_str().ok_or("`test.file` is required")?);
        let root = crate::agent::protocol::project_root(&file)
            .ok_or_else(|| format!("{} isn't in a Gradle project", file.display()))?;
        let piece = |key: &str| test[key].as_str().unwrap_or("").trim().to_owned();
        let filter = test_filter(
            &piece("package"),
            &piece("outerClass"),
            &piece("class"),
            &piece("method"),
        );
        if filter.is_empty() {
            return Err("the test has no class".to_owned());
        }
        return Ok((
            Launch::GradleTests {
                project: debugger::launch::gradle_module(&file, &root),
                root: root.clone(),
                filter,
            },
            root,
        ));
    }
    let program = arguments["program"]
        .as_str()
        .ok_or("give `test`, `program` or `request: attach`")?;
    let directory = arguments["cwd"]
        .as_str()
        .map_or_else(|| working_directory.to_path_buf(), PathBuf::from);
    let program_arguments: Vec<String> = arguments["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|argument| argument.as_str().map(str::to_owned))
        .collect();
    let environment: Vec<(String, String)> = arguments["env"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
        .collect();
    let name = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let launch = match name.as_str() {
        "gradlew" | "gradle" => Launch::Gradle {
            root: directory.clone(),
            arguments: program_arguments,
            environment,
        },
        "java" => Launch::Java {
            arguments: program_arguments,
            directory: directory.clone(),
            environment,
        },
        _ => {
            return Err(format!(
                "`{program}` isn't `java` or Gradle: kotlin-analyzer debugs JVM programs"
            ));
        }
    };
    let root = crate::agent::protocol::project_root(&directory).unwrap_or(directory);
    Ok((launch, root))
}

/// Gradle's `--tests` filter of a runnable's captures: `pkg.Outer$Class.method`,
/// backticks dropped, empty pieces left out.
fn test_filter(package: &str, outer_class: &str, class_name: &str, method: &str) -> String {
    if class_name.is_empty() {
        return String::new();
    }
    let classes = if outer_class.is_empty() {
        class_name.to_owned()
    } else {
        format!("{outer_class}${class_name}")
    };
    [package, &classes, method]
        .iter()
        .map(|piece| piece.replace('`', ""))
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runnables_captures_make_gradles_test_filter() {
        assert_eq!(
            test_filter("shop", "", "OrderTest", "`creates an order`"),
            "shop.OrderTest.creates an order"
        );
        assert_eq!(
            test_filter("shop", "OrderTest", "Refunds", ""),
            "shop.OrderTest$Refunds"
        );
        assert_eq!(test_filter("", "", "Loose", ""), "Loose");
        assert_eq!(test_filter("shop", "", "", "method"), "");
    }

    #[test]
    fn a_program_is_java_or_gradle_and_attaching_needs_a_port() {
        let directory = std::env::temp_dir();
        let (launch, _) = launch_of(
            &json!({
                "program": "/build/gradlew",
                "args": ["bootRun"],
                "cwd": "/build",
                "env": { "SPRING_PROFILES_ACTIVE": "local" },
            }),
            &directory,
        )
        .unwrap();
        assert_eq!(
            launch,
            Launch::Gradle {
                root: PathBuf::from("/build"),
                arguments: vec!["bootRun".to_owned()],
                environment: vec![("SPRING_PROFILES_ACTIVE".to_owned(), "local".to_owned())],
            }
        );
        let (launch, _) =
            launch_of(&json!({ "request": "attach", "port": 5005 }), &directory).unwrap();
        assert_eq!(
            launch,
            Launch::Attach {
                address: "localhost:5005".to_owned()
            }
        );
        assert!(launch_of(&json!({ "program": "node" }), &directory).is_err());
        assert!(launch_of(&json!({ "request": "attach" }), &directory).is_err());
    }
}
