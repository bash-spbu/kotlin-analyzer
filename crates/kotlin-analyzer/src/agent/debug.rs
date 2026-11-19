//! The MCP tools that let an agent debug: start a test (through Gradle)
//! or attach to a JVM, set breakpoints, run to the next stop, step, look
//! at the stack and variables, evaluate paths, read the program's output.
//! One session at a time, the agent's own — not an editor's.
//!
//! A stop is told as text: why, where (`path:line` and the line), the
//! innermost frames and the top frame's variables.

use std::path::{Path, PathBuf};
use std::time::Duration;

use debugger::{Launch, Session, Stop};
use jdwp::{StepDepth, ThreadId};
use serde_json::{Value, json};

/// How long a tool waits for the program to stop, unless told otherwise;
/// below an MCP client's own timeout.
const DEFAULT_WAIT: Duration = Duration::from_secs(50);
/// The longest wait a tool may ask for.
const LONGEST_WAIT: Duration = Duration::from_secs(600);
const FRAMES_SHOWN: usize = 6;

#[derive(Default)]
pub(crate) struct DebugTools {
    session: Option<DebugSession>,
}

struct DebugSession {
    session: Session,
    root: PathBuf,
    /// Running since the last tool resumed it: the next wait doesn't
    /// resume again.
    running: bool,
    thread: Option<ThreadId>,
}

impl DebugTools {
    /// The tools, as `tools/list` describes them.
    pub(crate) fn tools() -> Vec<Value> {
        let wait = json!({ "type": "integer", "minimum": 1, "maximum": 600, "description": "Seconds to wait for the program to stop (default 50). When it still runs, call `debug_continue` again to keep waiting." });
        let condition = json!({ "type": "string", "description": "Stop only when this holds: paths and literals compared with == != < <= > >=, joined by && || ! and parentheses — `item.count > 3 && order.id == \"A-1\"`. No calls." });
        let hit_condition = json!({ "type": "string", "description": "Stop only on these hits (of those the condition lets through): `5` (the fifth), `>= 5`, `% 10` (every tenth)." });
        let log_message = json!({ "type": "string", "description": "Don't stop: add this line to the output (`debug_output`) at each hit, paths in braces filled in — `total {order.total} for {order.id}`." });
        let location = json!({
            "type": "object",
            "properties": {
                "file": { "type": "string" },
                "line": { "type": "integer", "minimum": 1 },
                "condition": condition.clone(),
                "hitCondition": hit_condition.clone(),
                "logMessage": log_message.clone(),
            },
            "required": ["file", "line"],
        });
        vec![
            json!({
                "name": "debug_start",
                "description": "Start debugging: run JVM tests through Gradle with a debugger attached (`test`: Gradle's --tests filter, e.g. `shop.OrderTest` or `shop.OrderTest.creates an order`), or attach to a JVM listening for a debugger (`attach`: host:port, started with -agentlib:jdwp=transport=dt_socket,server=y,address=…). Sets the breakpoints, runs, and waits for the first stop: a breakpoint, an exception, or the end. One session at a time; starting another ends the previous.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "A file of the project (the test's file for `test`): picks the Gradle build and module." },
                        "test": { "type": "string" },
                        "attach": { "type": "string" },
                        "breakpoints": { "type": "array", "items": location },
                        "exceptions": { "type": "string", "enum": ["uncaught", "all", "none"], "description": "Which thrown exceptions stop the program (default `uncaught`)." },
                        "wait": wait.clone(),
                    },
                    "required": ["file"],
                },
            }),
            json!({
                "name": "debug_breakpoint",
                "description": "Set a breakpoint at a line of a Kotlin or Java file — with a condition, a hit count, or a message to log instead of stopping (a logpoint) — or remove the line's breakpoints with `remove`. A line of an inline function stops wherever it was inlined. Lists the breakpoints; a breakpoint not yet verified waits for its class to load.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file": { "type": "string" },
                        "line": { "type": "integer", "minimum": 1 },
                        "condition": condition,
                        "hitCondition": hit_condition,
                        "logMessage": log_message,
                        "remove": { "type": "boolean" },
                    },
                    "required": ["file", "line"],
                },
            }),
            json!({
                "name": "debug_continue",
                "description": "Resume the program and wait for the next stop (a breakpoint, an exception, the end).",
                "inputSchema": { "type": "object", "properties": { "wait": wait.clone() } },
            }),
            json!({
                "name": "debug_step",
                "description": "Step the stopped thread: `over` the current line, `into` the call on it, or `out` of the current function. Inlined library code (`forEach`'s body) is stepped through; stepping over a line where a suspend function suspends ends where its coroutine resumes, at the next line.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["over", "into", "out"] },
                        "wait": wait,
                    },
                    "required": ["kind"],
                },
            }),
            json!({
                "name": "debug_pause",
                "description": "Suspend the running program and show where its main thread is.",
                "inputSchema": { "type": "object", "properties": {} },
            }),
            json!({
                "name": "debug_stack",
                "description": "The stopped thread's whole call stack: frame number, function, file:line. An inline function's body is a frame of its own (`inlined`) above its caller; under a resumed coroutine come the suspend functions waiting for it (`suspended`), which aren't on the thread.",
                "inputSchema": { "type": "object", "properties": {} },
            }),
            json!({
                "name": "debug_variables",
                "description": "A frame's `this` and local variables (frame 0, the innermost, by default; numbers as `debug_stack` gives them) — or, with `expand`, the fields or elements of the value a path names (`order`, `order.items`). A suspended frame's are its coroutine's saved state.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "frame": { "type": "integer", "minimum": 0 },
                        "expand": { "type": "string" },
                    },
                },
            }),
            json!({
                "name": "debug_evaluate",
                "description": "The value of a path in a frame: a variable, `this` or a field of it, then `.field`, `[index]` (arrays, lists), `.length`/`.size` of an array. No calls.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "expression": { "type": "string" },
                        "frame": { "type": "integer", "minimum": 0 },
                    },
                    "required": ["expression"],
                },
            }),
            json!({
                "name": "debug_output",
                "description": "The program's (and Gradle's) latest output lines, logpoints' messages among them.",
                "inputSchema": { "type": "object", "properties": { "lines": { "type": "integer", "minimum": 1 } } },
            }),
            json!({
                "name": "debug_stop",
                "description": "End the debugged program and the session.",
                "inputSchema": { "type": "object", "properties": {} },
            }),
        ]
    }

    pub(crate) fn handles(name: &str) -> bool {
        name.starts_with("debug_")
    }

    /// A debug tool's answer, or why there is none.
    pub(crate) fn call(
        &mut self,
        name: &str,
        arguments: &Value,
        working_directory: &Path,
    ) -> Result<String, String> {
        if name == "debug_start" {
            return self.start(arguments, working_directory);
        }
        let debug = self
            .session
            .as_mut()
            .ok_or("no debug session: start one with `debug_start`")?;
        let wait = wait_of(arguments);
        match name {
            "debug_breakpoint" => {
                let file = absolute(
                    arguments["file"].as_str().ok_or("`file` is required")?,
                    working_directory,
                );
                let line = line_of(&arguments["line"])?;
                if arguments["remove"].as_bool() == Some(true) {
                    let ids: Vec<u32> = debug
                        .session
                        .breakpoints()
                        .into_iter()
                        .filter(|breakpoint| breakpoint.path == file && breakpoint.line == line)
                        .map(|breakpoint| breakpoint.id)
                        .collect();
                    if ids.is_empty() {
                        return Err("no breakpoint there".to_owned());
                    }
                    for id in ids {
                        debug.session.remove_breakpoint(id);
                    }
                } else {
                    debug
                        .session
                        .set_breakpoint_with(&file, line, options_of(arguments))?;
                }
                Ok(debug.breakpoint_list())
            }
            "debug_continue" => {
                if !debug.running {
                    debug.session.resume()?;
                    debug.running = true;
                }
                debug.wait(wait)
            }
            "debug_step" => {
                let thread = debug.stopped_thread()?;
                let depth = match arguments["kind"].as_str() {
                    Some("over") => StepDepth::Over,
                    Some("into") => StepDepth::Into,
                    Some("out") => StepDepth::Out,
                    _ => return Err("`kind` is `over`, `into` or `out`".to_owned()),
                };
                debug.running = true;
                let stop = debug.session.step(thread, depth, wait)?;
                debug.told(stop)
            }
            "debug_pause" => {
                let stop = debug.session.pause()?;
                debug.running = false;
                debug.thread = stop.thread();
                debug.describe(&stop)
            }
            "debug_stack" => {
                let thread = debug.stopped_thread()?;
                let frames = debug.session.stack(thread)?;
                Ok(frames
                    .iter()
                    .enumerate()
                    .map(|(number, frame)| format!("#{number} {}", debug.frame_line(frame)))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "debug_variables" => {
                let thread = debug.stopped_thread()?;
                let frame = debug.frame(thread, &arguments["frame"])?;
                let shown = match arguments["expand"].as_str() {
                    Some(path) => {
                        let value = debug.session.evaluate(thread, &frame, path)?;
                        let object = value.children.ok_or_else(|| {
                            format!("`{path}` = {} opens into nothing", value.value)
                        })?;
                        debug.session.children(object)?
                    }
                    None => debug.session.variables(thread, &frame)?,
                };
                if shown.is_empty() {
                    return Ok("No variables (compiled without debug information?)".to_owned());
                }
                Ok(shown
                    .iter()
                    .map(variable_line)
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "debug_evaluate" => {
                let thread = debug.stopped_thread()?;
                let frame = debug.frame(thread, &arguments["frame"])?;
                let expression = arguments["expression"]
                    .as_str()
                    .ok_or("`expression` is required")?;
                let value = debug.session.evaluate(thread, &frame, expression)?;
                Ok(variable_line(&value))
            }
            "debug_output" => {
                let lines = arguments["lines"].as_u64().unwrap_or(40) as usize;
                let output = debug.session.output_tail(lines);
                Ok(if output.is_empty() {
                    "No output".to_owned()
                } else {
                    output.join("\n")
                })
            }
            "debug_stop" => {
                if let Some(debug) = self.session.take() {
                    debug.session.terminate();
                }
                Ok("Stopped".to_owned())
            }
            unknown => Err(format!("no tool `{unknown}`")),
        }
    }

    fn start(&mut self, arguments: &Value, working_directory: &Path) -> Result<String, String> {
        if let Some(previous) = self.session.take() {
            previous.session.terminate();
        }
        let file = absolute(
            arguments["file"].as_str().ok_or("`file` is required")?,
            working_directory,
        );
        let root = super::protocol::project_root(&file)
            .ok_or_else(|| format!("{} isn't in a Gradle project", file.display()))?;
        let launch = match (arguments["test"].as_str(), arguments["attach"].as_str()) {
            (Some(filter), None) => Launch::GradleTests {
                project: debugger::launch::gradle_module(&file, &root),
                root: root.clone(),
                filter: filter.to_owned(),
            },
            (None, Some(address)) => Launch::Attach {
                address: address.to_owned(),
            },
            _ => {
                return Err("give `test` (a Gradle test filter) or `attach` (host:port)".to_owned());
            }
        };
        let session = Session::start(&launch, std::slice::from_ref(&root))?;
        let mut debug = DebugSession {
            session,
            root,
            running: false,
            thread: None,
        };
        let mut problems = Vec::new();
        for breakpoint in arguments["breakpoints"].as_array().into_iter().flatten() {
            let path = breakpoint["file"]
                .as_str()
                .map(|path| absolute(path, working_directory));
            let line = line_of(&breakpoint["line"]);
            match (path, line) {
                (Some(path), Ok(line)) => {
                    let options = options_of(breakpoint);
                    if let Err(error) = debug.session.set_breakpoint_with(&path, line, options) {
                        problems.push(error);
                    }
                }
                _ => problems.push(format!(
                    "a breakpoint needs `file` and `line`: {breakpoint}"
                )),
            }
        }
        let (caught, uncaught) = match arguments["exceptions"].as_str().unwrap_or("uncaught") {
            "all" => (true, true),
            "none" => (false, false),
            _ => (false, true),
        };
        debug.session.break_on_exceptions(caught, uncaught)?;
        debug.session.resume()?;
        debug.running = true;
        let told = debug.wait(wait_of(arguments));
        self.session = Some(debug);
        let mut answer = told?;
        if !problems.is_empty() {
            answer = format!("{}\n\n{answer}", problems.join("\n"));
        }
        Ok(answer)
    }
}

impl DebugSession {
    /// The frame numbered `number` in the stack (`debug_stack`'s
    /// numbers), the innermost by default.
    fn frame(&mut self, thread: ThreadId, number: &Value) -> Result<debugger::Frame, String> {
        let number = number.as_u64().unwrap_or(0) as usize;
        let mut frames = self.session.stack(thread)?;
        if number >= frames.len() {
            return Err(format!("no frame {number}: the stack has {}", frames.len()));
        }
        Ok(frames.swap_remove(number))
    }

    fn stopped_thread(&self) -> Result<ThreadId, String> {
        if self.running {
            return Err(
                "the program is running: wait with `debug_continue` or `debug_pause` it".to_owned(),
            );
        }
        self.thread
            .ok_or_else(|| "the program isn't stopped".to_owned())
    }

    fn wait(&mut self, patience: Duration) -> Result<String, String> {
        let stop = self.session.wait_for_stop(patience);
        self.told(stop)
    }

    fn told(&mut self, stop: Option<Stop>) -> Result<String, String> {
        let Some(stop) = stop else {
            self.running = true;
            return Ok(
                "Still running. `debug_continue` waits more; `debug_pause` suspends it.".to_owned(),
            );
        };
        self.running = false;
        self.thread = stop.thread();
        self.describe(&stop)
    }

    /// Why it stopped, where, the frames around, the variables there.
    fn describe(&mut self, stop: &Stop) -> Result<String, String> {
        let reason = match stop {
            Stop::Breakpoint { breakpoints, .. } => {
                let ids: Vec<String> = breakpoints.iter().map(u32::to_string).collect();
                format!("Stopped at breakpoint {}", ids.join(", "))
            }
            Stop::Step { .. } => "Stepped".to_owned(),
            Stop::Exception {
                exception, caught, ..
            } => {
                format!(
                    "Stopped on {} exception {exception}",
                    if *caught { "a caught" } else { "an uncaught" }
                )
            }
            Stop::Paused { .. } => "Paused".to_owned(),
            Stop::Ended { exit_code } => {
                let code = exit_code.map_or("unknown".to_owned(), |code| code.to_string());
                let output = self.session.output_tail(25).join("\n");
                return Ok(format!(
                    "The program ended (exit code {code}). Its last output:\n{output}"
                ));
            }
        };
        let thread = stop.thread().ok_or("no thread")?;
        let frames = self.session.stack(thread)?;
        let mut text = reason;
        if let Some(top) = frames.first() {
            text.push_str(&format!(" in {}", self.frame_line(top)));
            if let Some(source) = self.source_line(top) {
                text.push_str(&format!("\n    {source}"));
            }
        }
        // The project's frames: the libraries' (JUnit, reflection) only
        // counted.
        text.push_str("\n\nStack:");
        let in_project: Vec<(usize, &debugger::Frame)> = frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.path.is_some())
            .collect();
        for (number, frame) in in_project.iter().take(FRAMES_SHOWN) {
            text.push_str(&format!("\n  #{number} {}", self.frame_line(frame)));
        }
        let hidden = frames.len() - in_project.len().min(FRAMES_SHOWN);
        if hidden > 0 {
            text.push_str(&format!(
                "\n  … {hidden} more frames, in libraries mostly (`debug_stack`)"
            ));
        }
        let variables = match frames.first() {
            Some(top) => self.session.variables(thread, top).unwrap_or_default(),
            None => Vec::new(),
        };
        if !variables.is_empty() {
            text.push_str("\n\nVariables:");
            for variable in &variables {
                text.push_str(&format!("\n  {}", variable_line(variable)));
            }
        }
        Ok(text)
    }

    /// `shop/Checkout.kt:12 checkout (shop.CheckoutKt)`.
    fn frame_line(&self, frame: &debugger::Frame) -> String {
        let place = match (&frame.path, frame.line) {
            (Some(path), Some(line)) => format!(
                "{}:{line}",
                path.strip_prefix(&self.root).unwrap_or(path).display()
            ),
            (None, Some(line)) => format!("{}:{line}", frame.source_name.as_deref().unwrap_or("?")),
            _ => "(no line)".to_owned(),
        };
        let kind = match frame.kind {
            debugger::FrameKind::Code => "",
            debugger::FrameKind::Inlined { .. } => " [inlined]",
            debugger::FrameKind::Async { .. } => " [suspended]",
        };
        format!("{place} {} ({}){kind}", frame.method, frame.class_name)
    }

    /// The frame's line of source, numbered.
    fn source_line(&self, frame: &debugger::Frame) -> Option<String> {
        let path = frame.path.as_ref()?;
        let line = usize::try_from(frame.line?).ok()?;
        let text = std::fs::read_to_string(path).ok()?;
        let source = text.lines().nth(line.checked_sub(1)?)?;
        Some(format!("{line} | {}", source.trim_end()))
    }

    fn breakpoint_list(&self) -> String {
        let breakpoints = self.session.breakpoints();
        if breakpoints.is_empty() {
            return "No breakpoints".to_owned();
        }
        breakpoints
            .iter()
            .map(|breakpoint| {
                let options = &breakpoint.options;
                let checks: String = [
                    options
                        .condition
                        .as_ref()
                        .map(|condition| format!(" if {condition}")),
                    options
                        .hit_condition
                        .as_ref()
                        .map(|hits| format!(" on hits {hits}")),
                    options
                        .log_message
                        .as_ref()
                        .map(|message| format!(" logs \"{message}\"")),
                ]
                .into_iter()
                .flatten()
                .collect();
                format!(
                    "{}: {}:{}{checks}{}",
                    breakpoint.id,
                    breakpoint
                        .path
                        .strip_prefix(&self.root)
                        .unwrap_or(&breakpoint.path)
                        .display(),
                    breakpoint.line,
                    if breakpoint.verified {
                        ""
                    } else {
                        " (waiting for its class to load)"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A breakpoint's `condition`, `hitCondition` and `logMessage`.
fn options_of(arguments: &Value) -> debugger::BreakpointOptions {
    let text = |key: &str| arguments[key].as_str().map(str::to_owned);
    debugger::BreakpointOptions {
        condition: text("condition"),
        hit_condition: text("hitCondition"),
        log_message: text("logMessage"),
    }
}

fn variable_line(variable: &debugger::Shown) -> String {
    let type_name = if variable.type_name.is_empty() {
        String::new()
    } else {
        format!(": {}", variable.type_name)
    };
    let more = if variable.children.is_some() {
        " …"
    } else {
        ""
    };
    format!("{}{type_name} = {}{more}", variable.name, variable.value)
}

fn wait_of(arguments: &Value) -> Duration {
    arguments["wait"].as_u64().map_or(DEFAULT_WAIT, |seconds| {
        Duration::from_secs(seconds).min(LONGEST_WAIT)
    })
}

fn line_of(value: &Value) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|line| u32::try_from(line).ok())
        .filter(|line| *line > 0)
        .ok_or_else(|| "`line` is a number from 1".to_owned())
}

fn absolute(path: &str, working_directory: &Path) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        working_directory.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_breakpoints_checks_come_from_its_arguments() {
        let options = options_of(&json!({
            "file": "Order.kt",
            "line": 3,
            "condition": "count > 3",
            "hitCondition": "% 2",
            "logMessage": "count {count}",
        }));
        assert_eq!(
            options,
            debugger::BreakpointOptions {
                condition: Some("count > 3".to_owned()),
                hit_condition: Some("% 2".to_owned()),
                log_message: Some("count {count}".to_owned()),
            }
        );
        assert_eq!(
            options_of(&json!({ "file": "Order.kt", "line": 3 })),
            debugger::BreakpointOptions::default()
        );
    }

    #[test]
    fn every_breakpoint_location_takes_the_checks() {
        let tools = DebugTools::tools();
        for name in ["debug_start", "debug_breakpoint"] {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            let schema = if name == "debug_start" {
                &tool["inputSchema"]["properties"]["breakpoints"]["items"]["properties"]
            } else {
                &tool["inputSchema"]["properties"]
            };
            for key in ["condition", "hitCondition", "logMessage"] {
                assert!(schema.get(key).is_some(), "{name} lacks {key}");
            }
        }
    }
}
