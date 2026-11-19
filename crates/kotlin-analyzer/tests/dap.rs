//! `kotlin-analyzer dap` as an editor drives it: requests over stdio
//! against a small Java program compiled by `javac` (skipped where there
//! is none).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const PROGRAM: &str = r#"package demo;

public class Main {
    public static void main(String[] arguments) {
        String name = "kotlin";
        int count = 3;
        System.out.println("hello " + name);
        int doubled = count * 2;
        System.out.println("doubled " + doubled);
        System.out.println("given " + System.getenv("KOTLIN_ANALYZER_GIVEN"));
    }
}
"#;

const PATIENCE: Duration = Duration::from_secs(60);

fn line_of(text: &str) -> u64 {
    PROGRAM
        .lines()
        .position(|line| line.contains(text))
        .unwrap() as u64
        + 1
}

/// The program compiled with debug information, or `None` without `javac`.
fn compiled() -> Option<(PathBuf, PathBuf)> {
    let root = std::env::temp_dir().join(format!("kotlin-analyzer-dap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let sources = root.join("src/demo");
    std::fs::create_dir_all(&sources).unwrap();
    let file = sources.join("Main.java");
    std::fs::write(&file, PROGRAM).unwrap();
    let compiled = Command::new("javac")
        .args(["-g", "-d"])
        .arg(root.join("classes"))
        .arg(&file)
        .status()
        .ok()?;
    assert!(compiled.success(), "javac failed");
    Some((root, file))
}

/// The adapter's process and its messages, read on another thread.
struct Adapter {
    child: Child,
    input: ChildStdin,
    messages: Receiver<Value>,
    sequence: i64,
    /// Events seen while waiting for something else.
    seen: Vec<Value>,
}

impl Adapter {
    fn start() -> Adapter {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kotlin-analyzer"))
            .arg("dap")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, messages) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("Content-Length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if sender.send(serde_json::from_slice(&body).unwrap()).is_err() {
                    return;
                }
            }
        });
        Adapter {
            child,
            input,
            messages,
            sequence: 0,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, command: &str, arguments: Value) -> i64 {
        self.sequence += 1;
        let body = json!({ "seq": self.sequence, "type": "request", "command": command, "arguments": arguments })
            .to_string();
        write!(self.input, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.input.flush().unwrap();
        self.sequence
    }

    fn next_message(&mut self, deadline: Instant) -> Value {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.messages.recv_timeout(left) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("timed out; seen {:#?}", self.seen),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the adapter quit; seen {:#?}", self.seen)
            }
        }
    }

    /// Sends a request and returns its response's body, which must succeed.
    fn request(&mut self, command: &str, arguments: Value) -> Value {
        let sequence = self.send(command, arguments);
        let deadline = Instant::now() + PATIENCE;
        loop {
            let message = self.next_message(deadline);
            if message["type"] == "response" && message["request_seq"] == sequence {
                assert_eq!(message["success"], true, "{command} failed: {message}");
                return message["body"].clone();
            }
            self.seen.push(message);
        }
    }

    /// The next event named `event`, seen earlier or yet to come.
    fn event(&mut self, event: &str) -> Value {
        if let Some(index) = self
            .seen
            .iter()
            .position(|message| message["event"] == event)
        {
            return self.seen.remove(index)["body"].clone();
        }
        let deadline = Instant::now() + PATIENCE;
        loop {
            let message = self.next_message(deadline);
            if message["event"] == event {
                return message["body"].clone();
            }
            self.seen.push(message);
        }
    }

    fn output(&self) -> String {
        self.seen
            .iter()
            .filter(|message| message["event"] == "output")
            .filter_map(|message| message["body"]["output"].as_str())
            .collect()
    }

    /// The top frame of `thread` and its variables, as `name = value`.
    fn top_frame(&mut self, thread: &Value) -> (Value, Vec<String>) {
        let stack = self.request("stackTrace", json!({ "threadId": thread }));
        let frame = stack["stackFrames"][0].clone();
        let scopes = self.request("scopes", json!({ "frameId": frame["id"] }));
        let reference = scopes["scopes"][0]["variablesReference"].clone();
        let variables = self.request("variables", json!({ "variablesReference": reference }));
        let shown = variables["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|variable| {
                format!(
                    "{} = {}",
                    variable["name"].as_str().unwrap(),
                    variable["value"].as_str().unwrap()
                )
            })
            .collect();
        (frame, shown)
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn an_editor_launches_stops_inspects_steps_and_finishes() {
    let Some((root, file)) = compiled() else {
        eprintln!("no javac: skipped");
        return;
    };
    let mut adapter = Adapter::start();
    let capabilities = adapter.request("initialize", json!({ "adapterID": "Kotlin" }));
    assert_eq!(capabilities["supportsConfigurationDoneRequest"], true);
    assert_eq!(capabilities["supportsConditionalBreakpoints"], true);
    assert_eq!(capabilities["supportsLogPoints"], true);
    adapter.request(
        "launch",
        json!({
            "program": "java",
            "args": ["-cp", root.join("classes"), "demo.Main"],
            "cwd": root,
            "env": { "KOTLIN_ANALYZER_GIVEN": "an environment" },
        }),
    );
    adapter.event("initialized");
    let breakpoints = adapter.request(
        "setBreakpoints",
        json!({ "source": { "path": file }, "breakpoints": [
            // A logpoint: logs, doesn't stop.
            { "line": line_of("\"hello \""), "logMessage": "name is {name}, count {count}" },
            { "line": line_of("int doubled"), "condition": "count == 3 && name == \"kotlin\"" },
            // Never holds: the step that ends on its line stops as a step.
            { "line": line_of("\"doubled \""), "condition": "count > 100" },
        ] }),
    );
    assert_eq!(breakpoints["breakpoints"].as_array().unwrap().len(), 3);
    adapter.request(
        "setExceptionBreakpoints",
        json!({ "filters": ["uncaught"] }),
    );
    adapter.request("configurationDone", json!({}));

    let stopped = adapter.event("stopped");
    assert_eq!(stopped["reason"], "breakpoint");
    let thread = stopped["threadId"].clone();
    let threads = adapter.request("threads", json!({}));
    assert!(
        threads["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|listed| listed["id"] == thread)
    );
    let (frame, variables) = adapter.top_frame(&thread);
    assert_eq!(frame["line"], line_of("int doubled"));
    assert_eq!(
        frame["source"]["path"],
        json!(file),
        "mapped back to the file"
    );
    assert!(
        variables.contains(&"name = \"kotlin\"".to_owned()),
        "{variables:?}"
    );
    assert!(variables.contains(&"count = 3".to_owned()), "{variables:?}");
    let evaluated = adapter.request(
        "evaluate",
        json!({ "expression": "count", "frameId": frame["id"] }),
    );
    assert_eq!(evaluated["result"], "3");

    adapter.request("next", json!({ "threadId": thread }));
    let stopped = adapter.event("stopped");
    assert_eq!(stopped["reason"], "step");
    let (frame, variables) = adapter.top_frame(&thread);
    assert_eq!(frame["line"], line_of("\"doubled \""));
    assert!(
        variables.contains(&"doubled = 6".to_owned()),
        "{variables:?}"
    );

    adapter.request("continue", json!({ "threadId": thread }));
    let exited = adapter.event("exited");
    assert_eq!(exited["exitCode"], 0);
    adapter.event("terminated");
    let output = adapter.output();
    assert!(output.contains("hello kotlin\n"), "{output:?}");
    assert!(output.contains("doubled 6\n"), "{output:?}");
    assert!(output.contains("given an environment\n"), "{output:?}");
    assert!(output.contains("name is kotlin, count 3\n"), "{output:?}");
    adapter.request("disconnect", json!({}));
    let _ = std::fs::remove_dir_all(&root);
}
