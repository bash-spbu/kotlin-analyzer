//! A JVM debugger for Kotlin and Java, over JDWP: what the DAP adapter
//! and the agents' MCP tools drive.
//!
//! A [`Session`] starts or reaches the JVM (`launch`), sets breakpoints by
//! file and line (`breakpoints`) — with a condition, a hit count or a
//! message to log instead — and, once the JVM stops, tells where: frames
//! mapped back to the project's files, an inline function's body as a
//! frame of its own (`smap`), the functions a resumed coroutine's callers
//! wait in (`coroutines`). It shows variables (`values`), evaluates paths
//! (`order.items[0].price`) and steps — through inlined code it has no
//! source of, and over a suspension to where the coroutine resumes. Every
//! stop suspends the whole VM.

mod breakpoints;
pub mod conditions;
pub mod coroutines;
pub mod launch;
mod notices;
pub mod smap;
pub mod sources;
pub mod values;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jdwp::{
    Connection, EventKind, Location, MethodId, Modifier, ObjectId, ReferenceTypeId, StepDepth,
    SuspendPolicy, Tag, ThreadId, Value,
};

pub use breakpoints::{Breakpoint, BreakpointOptions};
use breakpoints::{BreakpointEntry, BreakpointTable, Checks};
use conditions::{Condition, LogMessage, Operand};
use coroutines::{Coroutines, Waiting};
pub use launch::Launch;
use notices::Notice;
use smap::{Origin, SourceMap};
use sources::{SourceIndex, SourceKey};
use values::Inspector;
pub use values::Shown;

/// Classes a step doesn't stop in: the JDK's and Kotlin's own.
const STEP_EXCLUDED: &[&str] = &[
    "java.*",
    "javax.*",
    "jdk.*",
    "sun.*",
    "kotlin.*",
    "kotlinx.coroutines.*",
];

/// How many more steps a step takes through code it doesn't stop in
/// before it stops anyway.
const STEP_LIMIT: usize = 1000;

/// Why the JVM stopped, or that it ended.
#[derive(Clone, PartialEq, Debug)]
pub enum Stop {
    Breakpoint {
        thread: ThreadId,
        breakpoints: Vec<u32>,
    },
    Step {
        thread: ThreadId,
    },
    Exception {
        thread: ThreadId,
        exception: String,
        caught: bool,
    },
    Paused {
        thread: ThreadId,
    },
    /// The JVM is gone; the process's exit code when it ended.
    Ended {
        exit_code: Option<i32>,
    },
}

impl Stop {
    pub fn thread(&self) -> Option<ThreadId> {
        match self {
            Stop::Breakpoint { thread, .. }
            | Stop::Step { thread }
            | Stop::Exception { thread, .. }
            | Stop::Paused { thread } => Some(*thread),
            Stop::Ended { .. } => None,
        }
    }
}

/// What a frame of a stopped thread's stack is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FrameKind {
    /// A method's code.
    Code,
    /// An inline function's body, compiled into the next frame's method:
    /// `function` as Kotlin's markers name it, when they do.
    Inlined { function: Option<String> },
    /// A suspend function waiting for the coroutine above to finish:
    /// suspended in `continuation`, not on the thread.
    Async { continuation: ObjectId },
}

/// A frame of a stopped thread's stack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    /// The thread's frame whose variables it has (for an async frame, the
    /// one it was found under).
    pub index: usize,
    pub kind: FrameKind,
    /// `shop.Order`.
    pub class_name: String,
    pub method: String,
    /// The project's file it is in, when found.
    pub path: Option<PathBuf>,
    /// `Order.kt`.
    pub source_name: Option<String>,
    pub line: Option<i32>,
}

/// What the event thread and the session share.
struct Shared {
    connection: Arc<Connection>,
    breakpoints: Mutex<BreakpointTable>,
}

/// Where a step began.
#[derive(Clone, Debug)]
struct StepOrigin {
    class: ReferenceTypeId,
    method: MethodId,
    line: Option<i32>,
    /// The method's first line: where a resumed suspend function enters
    /// before it jumps back to where it was.
    prologue_line: Option<i32>,
    /// The coroutine's state, when the method is a suspend function's.
    continuation: Option<ObjectId>,
    /// The coroutine has resumed: the step ends at the method's next line.
    resumed: bool,
    /// The line an inline function's body the step began in was inlined
    /// at: stepping over stays in that body.
    inlined_at: Option<u32>,
}

enum Stepping {
    Idle,
    /// A step of a line; `exit_request` watches the method suspend.
    Line {
        thread: ThreadId,
        depth: StepDepth,
        request: i32,
        exit_request: Option<i32>,
        origin: StepOrigin,
        steps: usize,
    },
    /// The coroutine suspended mid-step: waiting for it to resume
    /// (`request` stops in its `invokeSuspend`).
    AwaitingResume {
        depth: StepDepth,
        request: i32,
        origin: StepOrigin,
    },
}

/// Whether a notice stops the program.
enum Verdict {
    Stop(Stop),
    RunOn,
}

/// What a line of code a step reached is to the step.
enum Reached {
    /// Where it may stop.
    Code,
    /// Code it steps through: another step, at this depth.
    StepAgain(StepDepth),
}

/// What a line a step reached is.
enum LineKind {
    Own,
    /// An inline function's body, inlined at `call_line`.
    Inlined {
        call_line: u32,
        has_source: bool,
    },
    Elsewhere {
        has_source: bool,
    },
}

pub struct Session {
    shared: Arc<Shared>,
    debuggee: launch::Debuggee,
    notices: Receiver<Notice>,
    sources: SourceIndex,
    inspector: Inspector,
    coroutines: Coroutines,
    exception_request: Option<i32>,
    stepping: Stepping,
    /// The thread of the latest stop.
    current_thread: Option<ThreadId>,
    ended: bool,
    methods: HashMap<ReferenceTypeId, Vec<jdwp::Method>>,
    source_maps: HashMap<ReferenceTypeId, Option<Arc<SourceMap>>>,
    /// `None`: compiled without variables.
    variable_tables: HashMap<(ReferenceTypeId, MethodId), Option<Arc<Vec<jdwp::Variable>>>>,
}

impl Session {
    /// Starts or reaches the JVM; returns once it has connected, still
    /// suspended — set breakpoints, then [`Session::resume`]. Frames map
    /// to files under `source_roots`.
    pub fn start(launch: &Launch, source_roots: &[PathBuf]) -> Result<Session, String> {
        Session::start_streaming(launch, source_roots, None)
    }

    /// [`Session::start`], each line the program prints (and each
    /// logpoint's message) sent to `lines` as it comes.
    pub fn start_streaming(
        launch: &Launch,
        source_roots: &[PathBuf],
        lines: Option<std::sync::mpsc::Sender<String>>,
    ) -> Result<Session, String> {
        let (stream, debuggee) = launch::start(launch, lines)?;
        let connection =
            Arc::new(Connection::open(stream).map_err(|error| format!("JDWP: {error}"))?);
        let events = connection.events().ok_or("events already taken")?;
        let shared = Arc::new(Shared {
            connection,
            breakpoints: Mutex::default(),
        });
        let (notice_sender, notices) = std::sync::mpsc::channel();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("debugger-events".to_owned())
                .spawn(move || notices::handle_events(&shared, &events, &notice_sender))
                .map_err(|error| error.to_string())?;
        }
        // An attached VM runs: suspend it, as a launched one is.
        if matches!(launch, Launch::Attach { .. }) {
            shared
                .connection
                .suspend_all()
                .map_err(|error| error.to_string())?;
        }
        Ok(Session {
            shared,
            debuggee,
            notices,
            sources: SourceIndex::of(source_roots),
            inspector: Inspector::default(),
            coroutines: Coroutines::default(),
            exception_request: None,
            stepping: Stepping::Idle,
            current_thread: None,
            ended: false,
            methods: HashMap::new(),
            source_maps: HashMap::new(),
            variable_tables: HashMap::new(),
        })
    }

    fn connection(&self) -> &Connection {
        &self.shared.connection
    }

    /// Stops at `line` of `path`: in its classes loaded now, and in those
    /// that load later.
    pub fn set_breakpoint(&mut self, path: &Path, line: u32) -> Result<Breakpoint, String> {
        self.set_breakpoint_with(path, line, BreakpointOptions::default())
    }

    /// A breakpoint that asks `options` before it stops — or logs instead.
    pub fn set_breakpoint_with(
        &mut self,
        path: &Path,
        line: u32,
        options: BreakpointOptions,
    ) -> Result<Breakpoint, String> {
        let checks = Checks::of(&options)?;
        let key = SourceKey::of(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let connection = self.shared.connection.clone();
        let prepare_requests = breakpoints::watch_classes(&connection, &key);
        let mut requests = HashMap::new();
        for (class, signature) in breakpoints::loaded_classes_of(&connection, &key) {
            for location in breakpoints::locations_at(&connection, class, &signature, &key, line) {
                if let Ok(request) = breakpoints::set_breakpoint_request(&connection, &location) {
                    requests.insert(location, request);
                }
            }
        }
        let mut table = self.shared.breakpoints.lock().map_err(|_| "poisoned")?;
        table.next_id += 1;
        let breakpoint = Breakpoint {
            id: table.next_id,
            path: path.to_path_buf(),
            line,
            verified: !requests.is_empty(),
            options,
        };
        table.entries.push(BreakpointEntry {
            breakpoint: breakpoint.clone(),
            key,
            checks,
            prepare_requests,
            requests,
        });
        Ok(breakpoint)
    }

    pub fn remove_breakpoint(&mut self, id: u32) -> bool {
        let Ok(mut table) = self.shared.breakpoints.lock() else {
            return false;
        };
        let Some(position) = table
            .entries
            .iter()
            .position(|entry| entry.breakpoint.id == id)
        else {
            return false;
        };
        let entry = table.entries.remove(position);
        let connection = &self.shared.connection;
        for request in entry.requests.values() {
            let _ = connection.clear_event_request(EventKind::Breakpoint, *request);
        }
        for request in entry.prepare_requests {
            let _ = connection.clear_event_request(EventKind::ClassPrepare, request);
        }
        true
    }

    pub fn breakpoints(&self) -> Vec<Breakpoint> {
        self.shared
            .breakpoints
            .lock()
            .map(|table| {
                table
                    .entries
                    .iter()
                    .map(|entry| entry.breakpoint.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stops when an exception is thrown: caught ones too when `caught`,
    /// those nothing catches when `uncaught`.
    pub fn break_on_exceptions(&mut self, caught: bool, uncaught: bool) -> Result<(), String> {
        if let Some(request) = self.exception_request.take() {
            let _ = self
                .connection()
                .clear_event_request(EventKind::Exception, request);
        }
        if !caught && !uncaught {
            return Ok(());
        }
        let request = self
            .connection()
            .set_event_request(
                EventKind::Exception,
                SuspendPolicy::All,
                &[Modifier::ExceptionOnly {
                    exception: None,
                    caught,
                    uncaught,
                }],
            )
            .map_err(|error| error.to_string())?;
        self.exception_request = Some(request);
        Ok(())
    }

    /// Lets the JVM run.
    pub fn resume(&mut self) -> Result<(), String> {
        if self.ended {
            return Err("the program has ended".to_owned());
        }
        self.inspector = Inspector::default();
        self.connection()
            .resume_all()
            .map_err(|error| error.to_string())
    }

    /// Waits for the JVM to stop or end; `None` if it still runs after
    /// `patience`. Breakpoints whose checks fail, logpoints and steps
    /// through code they don't stop in run on meanwhile.
    pub fn wait_for_stop(&mut self, patience: Duration) -> Option<Stop> {
        let deadline = Instant::now() + patience;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let notice = match self.notices.recv_timeout(left) {
                Ok(notice) => notice,
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => {
                    Notice::Stop(Stop::Ended { exit_code: None })
                }
            };
            if let Verdict::Stop(stop) = self.consider(notice) {
                return Some(self.stopped(stop));
            }
        }
    }

    fn stopped(&mut self, stop: Stop) -> Stop {
        self.clear_stepping();
        let stop = match stop {
            Stop::Ended { .. } => {
                self.ended = true;
                Stop::Ended {
                    exit_code: self.debuggee.wait(Duration::from_secs(60)),
                }
            }
            stopped => stopped,
        };
        self.current_thread = stop.thread();
        // A thread an event stopped can run `toString()`.
        self.inspector = Inspector::calling_in(stop.thread());
        stop
    }

    fn consider(&mut self, notice: Notice) -> Verdict {
        match notice {
            Notice::Stop(stop) => Verdict::Stop(stop),
            Notice::Breakpoint {
                thread,
                requests,
                stepped,
            } => self.at_breakpoint(thread, &requests, stepped),
            Notice::Step { thread } => self.after_step(thread),
            Notice::Exited {
                thread,
                request,
                class,
                method,
                suspended,
            } => self.after_exit(thread, request, (class, method), suspended),
        }
    }

    /// Lets the JVM run on after a notice that doesn't stop it.
    fn run_on(&mut self) -> Verdict {
        self.inspector = Inspector::default();
        let _ = self.connection().resume_all();
        Verdict::RunOn
    }

    fn at_breakpoint(&mut self, thread: ThreadId, requests: &[i32], stepped: bool) -> Verdict {
        let resumes_the_step = matches!(
            &self.stepping,
            Stepping::AwaitingResume { request, .. } if requests.contains(request)
        );
        let hit = self
            .shared
            .breakpoints
            .lock()
            .map(|table| table.of_requests(requests))
            .unwrap_or_default();
        self.inspector = Inspector::calling_in(Some(thread));
        let stopping: Vec<u32> = hit
            .into_iter()
            .filter(|id| self.breakpoint_stops(*id, thread))
            .collect();
        if !stopping.is_empty() {
            return Verdict::Stop(Stop::Breakpoint {
                thread,
                breakpoints: stopping,
            });
        }
        if resumes_the_step {
            return self.continue_resumed_step(thread);
        }
        if stepped {
            return self.after_step(thread);
        }
        self.run_on()
    }

    /// Whether breakpoint `id`, hit in `thread`, stops: its condition
    /// holds, its hit count is met, and it doesn't log instead. A condition
    /// that can't be evaluated stops, saying why.
    fn breakpoint_stops(&mut self, id: u32, thread: ThreadId) -> bool {
        let (condition, log_message) = {
            let Ok(mut table) = self.shared.breakpoints.lock() else {
                return true;
            };
            let Some(entry) = table.entry_mut(id) else {
                return false;
            };
            (
                entry.checks.condition.clone(),
                entry.checks.log_message.clone(),
            )
        };
        if let Some(condition) = condition {
            match self.condition_holds(thread, &condition) {
                Ok(true) => {}
                Ok(false) => return false,
                Err(why) => {
                    self.debuggee
                        .note(format!("breakpoint {id}: the condition failed: {why}"));
                    return true;
                }
            }
        }
        let hit_count_met = {
            let Ok(mut table) = self.shared.breakpoints.lock() else {
                return true;
            };
            let Some(entry) = table.entry_mut(id) else {
                return false;
            };
            entry.checks.hits += 1;
            let hits = entry.checks.hits;
            entry
                .checks
                .hit_condition
                .is_none_or(|hit_condition| hit_condition.is_met(hits))
        };
        if !hit_count_met {
            return false;
        }
        match log_message {
            Some(message) => {
                let text = self.render_log(thread, &message);
                self.debuggee.note(text);
                false
            }
            None => true,
        }
    }

    fn condition_holds(&mut self, thread: ThreadId, condition: &Condition) -> Result<bool, String> {
        let frame = self.top_frame(thread)?;
        condition.holds(&mut |path| {
            let value = self.path_value(thread, &frame, path)?;
            Ok(self.operand_of(value))
        })
    }

    fn render_log(&mut self, thread: ThreadId, message: &LogMessage) -> String {
        let frame = match self.top_frame(thread) {
            Ok(frame) => frame,
            Err(why) => return format!("<{why}>"),
        };
        message.render(&mut |path| {
            let value = self.path_value(thread, &frame, path)?;
            let connection = self.shared.connection.clone();
            Ok(match value {
                // Text as itself, not quoted.
                Value::Object {
                    tag: Tag::String,
                    id,
                } => connection
                    .string_value(id)
                    .map_err(|error| error.to_string())?,
                value => self.inspector.show(&connection, path, value).value,
            })
        })
    }

    /// A value as a condition compares it: text, numbers and booleans by
    /// value (boxed ones too), other objects by identity.
    fn operand_of(&mut self, value: Value) -> Operand {
        let connection = self.shared.connection.clone();
        match value {
            Value::Byte(number) => Operand::Integer(i64::from(number)),
            Value::Short(number) => Operand::Integer(i64::from(number)),
            Value::Int(number) => Operand::Integer(i64::from(number)),
            Value::Long(number) => Operand::Integer(number),
            Value::Float(number) => Operand::Decimal(f64::from(number)),
            Value::Double(number) => Operand::Decimal(number),
            Value::Boolean(value) => Operand::Boolean(value),
            Value::Char(unit) => Operand::Char(
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
            ),
            Value::Null | Value::Void => Operand::Null,
            Value::Object {
                tag: Tag::String,
                id,
            } => connection
                .string_value(id)
                .map_or(Operand::Reference(id.0), Operand::Text),
            Value::Object { id, .. } => match self.unboxed(id) {
                Some(value) => self.operand_of(value),
                None => Operand::Reference(id.0),
            },
        }
    }

    /// The primitive a boxed `Integer`, `Long`… holds.
    fn unboxed(&mut self, object: ObjectId) -> Option<Value> {
        let connection = self.shared.connection.clone();
        let class = connection.reference_type(object).ok()?;
        let signature = self.inspector.signature(&connection, class).ok()?;
        let is_boxed = values::BOXED.contains(&signature.as_str());
        if !is_boxed {
            return None;
        }
        self.inspector
            .field_value(&connection, object, "value")
            .ok()
            .flatten()
    }

    /// Resumes and waits for the next stop.
    pub fn continue_to_stop(&mut self, patience: Duration) -> Result<Option<Stop>, String> {
        self.resume()?;
        Ok(self.wait_for_stop(patience))
    }

    /// Steps `thread` over, into or out of a line, and waits for it to
    /// stop.
    pub fn step(
        &mut self,
        thread: ThreadId,
        depth: StepDepth,
        patience: Duration,
    ) -> Result<Option<Stop>, String> {
        self.begin_step(thread, depth)?;
        Ok(self.wait_for_stop(patience))
    }

    /// Starts a step of `thread` and lets the JVM run: the next stop (from
    /// [`Session::wait_for_stop`]) ends it. A step over a line where a
    /// suspend function suspends ends where its coroutine resumes, at the
    /// function's next line.
    pub fn begin_step(&mut self, thread: ThreadId, depth: StepDepth) -> Result<(), String> {
        self.clear_stepping();
        let origin = self.step_origin(thread)?;
        let request = self.line_step_request(thread, depth)?;
        let exit_request = match (depth, origin.continuation) {
            (StepDepth::Over, Some(_)) => self.exit_request(thread, origin.class).ok(),
            _ => None,
        };
        self.stepping = Stepping::Line {
            thread,
            depth,
            request,
            exit_request,
            origin,
            steps: 0,
        };
        self.resume()
    }

    fn line_step_request(&self, thread: ThreadId, depth: StepDepth) -> Result<i32, String> {
        let mut modifiers = vec![Modifier::Step { thread, depth }, Modifier::Count(1)];
        modifiers.extend(
            STEP_EXCLUDED
                .iter()
                .map(|pattern| Modifier::ClassExclude((*pattern).to_owned())),
        );
        self.connection()
            .set_event_request(EventKind::SingleStep, SuspendPolicy::All, &modifiers)
            .map_err(|error| error.to_string())
    }

    /// Watches the methods of `class` return in `thread`: whether the
    /// stepped one suspends.
    fn exit_request(&mut self, thread: ThreadId, class: ReferenceTypeId) -> Result<i32, String> {
        let connection = self.shared.connection.clone();
        let signature = self
            .inspector
            .signature(&connection, class)
            .map_err(|error| error.to_string())?;
        let class_name = sources::class_name(&signature).ok_or("not a class")?;
        connection
            .set_event_request(
                EventKind::MethodExit,
                SuspendPolicy::All,
                &[
                    Modifier::ThreadOnly(thread),
                    Modifier::ClassMatch(class_name),
                ],
            )
            .map_err(|error| error.to_string())
    }

    /// Where `thread` is, as a step starting there sees it.
    fn step_origin(&mut self, thread: ThreadId) -> Result<StepOrigin, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let frame = frames.first().ok_or("no frames")?;
        let location = frame.location;
        let table = connection.line_table(location.class, location.method).ok();
        let line = table
            .as_ref()
            .and_then(|table| line_at(table, location.index));
        let prologue_line = table.as_ref().and_then(|table| {
            table
                .lines
                .iter()
                .min_by_key(|(index, _)| *index)
                .map(|(_, line)| *line)
        });
        let continuation = self.continuation_of(thread, frame);
        let inlined_at = match self.line_kind(&location) {
            LineKind::Inlined { call_line, .. } => Some(call_line),
            _ => None,
        };
        Ok(StepOrigin {
            class: location.class,
            method: location.method,
            line,
            prologue_line,
            continuation,
            resumed: false,
            inlined_at,
        })
    }

    /// The coroutine state of a frame: a suspend function's
    /// `$continuation`, or a suspend lambda's `this`.
    fn continuation_of(&mut self, thread: ThreadId, frame: &jdwp::StackFrame) -> Option<ObjectId> {
        let connection = self.shared.connection.clone();
        let location = frame.location;
        let variables = self.variable_table(location.class, location.method);
        let local = variables.as_ref().and_then(|variables| {
            variables
                .iter()
                .find(|variable| {
                    variable.name == "$continuation" && variable.is_live_at(location.index)
                })
                .cloned()
        });
        if let Some(local) = local {
            let values = connection
                .frame_values(thread, frame.id, &[(local.slot, Tag::Object)])
                .ok()?;
            return match values.first() {
                Some(Value::Object { id, .. }) => Some(*id),
                _ => None,
            };
        }
        self.resumed_continuation(thread, frame)
    }

    /// A continuation's `invokeSuspend` frame: its `this`.
    fn resumed_continuation(
        &mut self,
        thread: ThreadId,
        frame: &jdwp::StackFrame,
    ) -> Option<ObjectId> {
        let method = self.method(frame.location.class, frame.location.method)?;
        if method.name != "invokeSuspend" {
            return None;
        }
        let connection = self.shared.connection.clone();
        let this = connection.this_object(thread, frame.id).ok()??;
        let class = connection.reference_type(this).ok()?;
        self.coroutines
            .is_continuation(&connection, &mut self.inspector, class)
            .then_some(this)
    }

    /// A step reached a line: stops there, or steps on through code it
    /// doesn't stop in.
    fn after_step(&mut self, thread: ThreadId) -> Verdict {
        let Stepping::Line {
            depth,
            origin,
            steps,
            ..
        } = &self.stepping
        else {
            return Verdict::Stop(Stop::Step { thread });
        };
        let (depth, origin, steps) = (*depth, origin.clone(), *steps);
        if steps >= STEP_LIMIT {
            return Verdict::Stop(Stop::Step { thread });
        }
        let Some(location) = self.top_location(thread) else {
            return Verdict::Stop(Stop::Step { thread });
        };
        match self.reached(&location, depth, &origin) {
            Reached::Code => Verdict::Stop(Stop::Step { thread }),
            Reached::StepAgain(depth) => match self.step_again(thread, depth) {
                Ok(()) => Verdict::RunOn,
                Err(_) => Verdict::Stop(Stop::Step { thread }),
            },
        }
    }

    fn reached(&mut self, location: &Location, depth: StepDepth, origin: &StepOrigin) -> Reached {
        let in_origin_method = location.class == origin.class && location.method == origin.method;
        if origin.resumed {
            if !in_origin_method {
                return Reached::StepAgain(StepDepth::Into);
            }
            // It enters before its first line, passes its declaration's,
            // then the line it suspended at.
            let line = self.raw_line(location);
            let before_where_it_was =
                line.is_none() || line == origin.line || line == origin.prologue_line;
            return if before_where_it_was {
                Reached::StepAgain(StepDepth::Over)
            } else {
                Reached::Code
            };
        }
        // A suspend function returns from suspending on its declaration's
        // line: on to the exit, which tells.
        let leaves_suspended = origin.continuation.is_some()
            && in_origin_method
            && self.raw_line(location) == origin.prologue_line;
        if leaves_suspended {
            return Reached::StepAgain(StepDepth::Over);
        }
        let onward = match depth {
            StepDepth::Out => StepDepth::Over,
            depth => depth,
        };
        match self.line_kind(location) {
            LineKind::Inlined {
                call_line,
                has_source,
            } => {
                let entered = origin.inlined_at != Some(call_line);
                let steps_over_it = depth == StepDepth::Over && entered;
                if steps_over_it || !has_source {
                    Reached::StepAgain(onward)
                } else {
                    Reached::Code
                }
            }
            LineKind::Elsewhere { has_source: false } => Reached::StepAgain(onward),
            LineKind::Own | LineKind::Elsewhere { has_source: true } => Reached::Code,
        }
    }

    /// Replaces the step's request with a new one at `depth` and runs.
    fn step_again(&mut self, thread: ThreadId, depth: StepDepth) -> Result<(), String> {
        let request = self.line_step_request(thread, depth)?;
        if let Stepping::Line {
            request: current,
            steps,
            ..
        } = &mut self.stepping
        {
            let previous = std::mem::replace(current, request);
            *steps += 1;
            let _ = self
                .shared
                .connection
                .clear_event_request(EventKind::SingleStep, previous);
        }
        self.inspector = Inspector::default();
        self.connection()
            .resume_all()
            .map_err(|error| error.to_string())
    }

    /// A method of the stepped class returned: if it is the stepped
    /// suspend function suspending, the step waits for its coroutine to
    /// resume instead.
    fn after_exit(
        &mut self,
        thread: ThreadId,
        request: i32,
        method: (ReferenceTypeId, MethodId),
        suspended: bool,
    ) -> Verdict {
        let Stepping::Line {
            thread: stepping_thread,
            depth,
            request: step_request,
            exit_request: Some(exit_request),
            origin,
            ..
        } = &self.stepping
        else {
            return self.run_on();
        };
        let is_the_stepped_method = *exit_request == request
            && thread == *stepping_thread
            && method == (origin.class, origin.method);
        if !is_the_stepped_method {
            return self.run_on();
        }
        let (depth, step_request, exit_request, origin) =
            (*depth, *step_request, *exit_request, origin.clone());
        let connection = self.shared.connection.clone();
        let _ = connection.clear_event_request(EventKind::MethodExit, exit_request);
        let Some(continuation) = origin.continuation.filter(|_| suspended) else {
            if let Stepping::Line { exit_request, .. } = &mut self.stepping {
                *exit_request = None;
            }
            return self.run_on();
        };
        let _ = connection.clear_event_request(EventKind::SingleStep, step_request);
        match self.resume_request(continuation) {
            Ok(request) => {
                self.stepping = Stepping::AwaitingResume {
                    depth,
                    request,
                    origin,
                };
                self.run_on()
            }
            Err(_) => Verdict::Stop(Stop::Step { thread }),
        }
    }

    /// Stops where `continuation` resumes: its `invokeSuspend`, called on
    /// it.
    fn resume_request(&mut self, continuation: ObjectId) -> Result<i32, String> {
        let connection = self.shared.connection.clone();
        let class = connection
            .reference_type(continuation)
            .map_err(|error| error.to_string())?;
        let methods = connection
            .methods(class)
            .map_err(|error| error.to_string())?;
        let method = methods
            .iter()
            .find(|method| method.name == "invokeSuspend")
            .ok_or("no invokeSuspend")?;
        let table = connection
            .line_table(class, method.id)
            .map_err(|error| error.to_string())?;
        let location = Location {
            type_tag: jdwp::TypeTag::Class,
            class,
            method: method.id,
            index: table.start,
        };
        connection
            .set_event_request(
                EventKind::Breakpoint,
                SuspendPolicy::All,
                &[
                    Modifier::LocationOnly(location),
                    Modifier::InstanceOnly(continuation),
                ],
            )
            .map_err(|error| error.to_string())
    }

    /// The suspended coroutine resumed: on, in `thread`, to the stepped
    /// function's next line.
    fn continue_resumed_step(&mut self, thread: ThreadId) -> Verdict {
        let Stepping::AwaitingResume {
            depth,
            request,
            mut origin,
        } = std::mem::replace(&mut self.stepping, Stepping::Idle)
        else {
            return self.run_on();
        };
        let _ = self
            .shared
            .connection
            .clear_event_request(EventKind::Breakpoint, request);
        origin.resumed = true;
        match self.line_step_request(thread, StepDepth::Into) {
            Ok(request) => {
                self.stepping = Stepping::Line {
                    thread,
                    depth,
                    request,
                    exit_request: None,
                    origin,
                    steps: 0,
                };
                self.run_on()
            }
            Err(_) => Verdict::Stop(Stop::Step { thread }),
        }
    }

    /// Ends the step under way, if any, and its requests.
    fn clear_stepping(&mut self) {
        let connection = self.shared.connection.clone();
        match std::mem::replace(&mut self.stepping, Stepping::Idle) {
            Stepping::Idle => {}
            Stepping::Line {
                request,
                exit_request,
                ..
            } => {
                let _ = connection.clear_event_request(EventKind::SingleStep, request);
                if let Some(exit_request) = exit_request {
                    let _ = connection.clear_event_request(EventKind::MethodExit, exit_request);
                }
            }
            Stepping::AwaitingResume { request, .. } => {
                let _ = connection.clear_event_request(EventKind::Breakpoint, request);
            }
        }
    }

    /// Suspends the running JVM.
    pub fn pause(&mut self) -> Result<Stop, String> {
        self.clear_stepping();
        self.connection()
            .suspend_all()
            .map_err(|error| error.to_string())?;
        let thread = self.main_thread().ok_or("no thread to show")?;
        self.current_thread = Some(thread);
        Ok(Stop::Paused { thread })
    }

    /// The thread named `main`, else the first with frames in the
    /// project's code.
    fn main_thread(&mut self) -> Option<ThreadId> {
        let threads = self.connection().all_threads().ok()?;
        let named_main = threads.iter().copied().find(|thread| {
            self.connection()
                .thread_name(*thread)
                .is_ok_and(|name| name == "main")
        });
        named_main.or_else(|| threads.first().copied())
    }

    pub fn current_thread(&self) -> Option<ThreadId> {
        self.current_thread
    }

    /// Every thread, with its name.
    pub fn threads(&self) -> Result<Vec<(ThreadId, String)>, String> {
        let connection = self.connection();
        let threads = connection
            .all_threads()
            .map_err(|error| error.to_string())?;
        Ok(threads
            .into_iter()
            .map(|thread| (thread, connection.thread_name(thread).unwrap_or_default()))
            .collect())
    }

    /// The stopped thread's frames, the innermost first: an inline
    /// function's body as its own frame above its caller's, and under a
    /// resumed coroutine the functions waiting for it.
    pub fn stack(&mut self, thread: ThreadId) -> Result<Vec<Frame>, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let mut shown = Vec::with_capacity(frames.len());
        for (index, frame) in frames.iter().enumerate() {
            shown.extend(self.frames_at(index, &frame.location));
            if let Some(continuation) = self.resumed_continuation(thread, frame) {
                let waiting = self.coroutines.waiting_for(
                    &connection,
                    &mut self.inspector,
                    thread,
                    continuation,
                );
                for waiting in waiting {
                    shown.push(self.async_frame(index, waiting));
                }
            }
        }
        Ok(shown)
    }

    /// The innermost frame: an inline function's body, or the method's.
    pub fn top_frame(&mut self, thread: ThreadId) -> Result<Frame, String> {
        let location = self.top_location(thread).ok_or("no frames")?;
        self.frames_at(0, &location)
            .into_iter()
            .next()
            .ok_or_else(|| "no frames".to_owned())
    }

    fn top_location(&self, thread: ThreadId) -> Option<Location> {
        let frames = self.connection().frames(thread).ok()?;
        frames.first().map(|frame| frame.location)
    }

    /// The frames of the thread's frame `index`: its method's, and above
    /// it the inline function's body it is in, if it is.
    fn frames_at(&mut self, index: usize, location: &Location) -> Vec<Frame> {
        let frame = self.frame_at(index, location);
        let Some(raw_line) = frame.line.and_then(|line| u32::try_from(line).ok()) else {
            return vec![frame];
        };
        let Some(source_map) = self.source_map(location.class) else {
            return vec![frame];
        };
        match source_map.origin(raw_line) {
            None => vec![frame],
            Some(Origin::Own { line }) => vec![Frame {
                line: Some(line as i32),
                ..frame
            }],
            Some(Origin::Elsewhere { file, line }) => vec![Frame {
                path: self.sources.find(&file.name, &file.package()),
                source_name: Some(file.name.clone()),
                line: Some(line as i32),
                ..frame
            }],
            Some(Origin::Inlined {
                file,
                line,
                call_line,
            }) => {
                let function = self.inlined_function(location);
                let body = Frame {
                    index,
                    kind: FrameKind::Inlined {
                        function: function.clone(),
                    },
                    class_name: frame.class_name.clone(),
                    method: function.unwrap_or_else(|| "inlined".to_owned()),
                    path: self.sources.find(&file.name, &file.package()),
                    source_name: Some(file.name.clone()),
                    line: Some(line as i32),
                };
                vec![
                    body,
                    Frame {
                        line: Some(call_line as i32),
                        ..frame
                    },
                ]
            }
        }
    }

    /// The method's frame, its line as compiled.
    fn frame_at(&mut self, index: usize, location: &Location) -> Frame {
        let connection = self.shared.connection.clone();
        let signature = self
            .inspector
            .signature(&connection, location.class)
            .unwrap_or_default();
        let class_name = sources::class_name(&signature).unwrap_or_else(|| signature.clone());
        let method = self
            .method(location.class, location.method)
            .map(|method| method.name)
            .unwrap_or_default();
        let source_name = connection.source_file(location.class).ok();
        let line = self.raw_line(location);
        let package = package_of(&class_name);
        let path = source_name
            .as_deref()
            .and_then(|name| self.sources.find(name, &package));
        Frame {
            index,
            kind: FrameKind::Code,
            class_name,
            method,
            path,
            source_name,
            line,
        }
    }

    fn async_frame(&mut self, index: usize, waiting: Waiting) -> Frame {
        let package = package_of(&waiting.class_name);
        let path = waiting
            .file_name
            .as_deref()
            .and_then(|name| self.sources.find(name, &package));
        Frame {
            index,
            kind: FrameKind::Async {
                continuation: waiting.continuation,
            },
            class_name: waiting.class_name,
            method: waiting.method,
            path,
            source_name: waiting.file_name,
            line: waiting.line,
        }
    }

    fn raw_line(&self, location: &Location) -> Option<i32> {
        self.connection()
            .line_table(location.class, location.method)
            .ok()
            .and_then(|table| line_at(&table, location.index))
    }

    /// What the code at `location` is to a step: the class's own lines,
    /// an inlined body, or another file's code, and whether the project
    /// has its source.
    fn line_kind(&mut self, location: &Location) -> LineKind {
        let Some(raw_line) = self
            .raw_line(location)
            .and_then(|line| u32::try_from(line).ok())
        else {
            return LineKind::Own;
        };
        let Some(source_map) = self.source_map(location.class) else {
            return LineKind::Own;
        };
        match source_map.origin(raw_line) {
            None | Some(Origin::Own { .. }) => LineKind::Own,
            Some(Origin::Inlined {
                file, call_line, ..
            }) => LineKind::Inlined {
                call_line,
                has_source: self.sources.find(&file.name, &file.package()).is_some(),
            },
            Some(Origin::Elsewhere { file, .. }) => LineKind::Elsewhere {
                has_source: self.sources.find(&file.name, &file.package()).is_some(),
            },
        }
    }

    fn source_map(&mut self, class: ReferenceTypeId) -> Option<Arc<SourceMap>> {
        if let Some(known) = self.source_maps.get(&class) {
            return known.clone();
        }
        let source_map = self
            .connection()
            .source_debug_extension(class)
            .ok()
            .and_then(|text| SourceMap::parse(&text))
            .map(Arc::new);
        self.source_maps.insert(class, source_map.clone());
        source_map
    }

    fn variable_table(
        &mut self,
        class: ReferenceTypeId,
        method: MethodId,
    ) -> Option<Arc<Vec<jdwp::Variable>>> {
        if let Some(known) = self.variable_tables.get(&(class, method)) {
            return known.clone();
        }
        let table = self
            .connection()
            .variable_table(class, method)
            .ok()
            .map(Arc::new);
        self.variable_tables.insert((class, method), table.clone());
        table
    }

    /// The inline function whose body `location` is in: the innermost of
    /// Kotlin's `$i$f$<function>` markers live there.
    fn inlined_function(&mut self, location: &Location) -> Option<String> {
        let variables = self.variable_table(location.class, location.method)?;
        variables
            .iter()
            .filter(|variable| variable.is_live_at(location.index))
            .filter_map(|variable| {
                let function = variable.name.strip_prefix("$i$f$")?;
                Some((variable.start, function.to_owned()))
            })
            .max_by_key(|(start, _)| *start)
            .map(|(_, function)| function)
    }

    fn method(&mut self, class: ReferenceTypeId, method: MethodId) -> Option<jdwp::Method> {
        if !self.methods.contains_key(&class) {
            let methods = self.connection().methods(class).ok()?;
            self.methods.insert(class, methods);
        }
        self.methods
            .get(&class)?
            .iter()
            .find(|candidate| candidate.id == method)
            .cloned()
    }

    /// A frame's variables: a method's `this` and live locals, an inline
    /// function body's own locals, an async frame's saved state.
    pub fn variables(&mut self, thread: ThreadId, frame: &Frame) -> Result<Vec<Shown>, String> {
        let connection = self.shared.connection.clone();
        if let FrameKind::Async { continuation } = frame.kind {
            return self
                .inspector
                .children(&connection, continuation)
                .map_err(|error| error.to_string());
        }
        let named = self.named_values(thread, frame)?;
        Ok(named
            .into_iter()
            .map(|(name, value)| self.inspector.show(&connection, &name, value))
            .collect())
    }

    /// The variables of a method's or inlined body's frame, by the names
    /// the source gives them. Kotlin marks an inlined body's locals `$iv`:
    /// they are the body's, the others its caller's.
    fn named_values(
        &mut self,
        thread: ThreadId,
        frame: &Frame,
    ) -> Result<Vec<(String, Value)>, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let jvm_frame = frames.get(frame.index).ok_or("no such frame")?;
        let is_inlined_body = matches!(frame.kind, FrameKind::Inlined { .. });
        let mut named = Vec::new();
        if !is_inlined_body && let Ok(Some(this)) = connection.this_object(thread, jvm_frame.id) {
            named.push((
                "this".to_owned(),
                Value::Object {
                    tag: Tag::Object,
                    id: this,
                },
            ));
        }
        let location = jvm_frame.location;
        let Some(variables) = self.variable_table(location.class, location.method) else {
            return Ok(named);
        };
        let mut live = Vec::new();
        for variable in variables
            .iter()
            .filter(|variable| variable.is_live_at(location.index))
        {
            // Kotlin's markers of inlined code, not the program's; `this`
            // is first already.
            let is_marker = variable.name.starts_with("$i$") || variable.name.is_empty();
            if is_marker || variable.name == "this" {
                continue;
            }
            let body_local = variable.name.strip_suffix("$iv");
            let name = match (is_inlined_body, body_local) {
                (true, Some(name)) => name.to_owned(),
                (false, None) => variable.name.clone(),
                _ => continue,
            };
            live.push((name, variable.slot, Tag::of_signature(&variable.signature)));
        }
        let slots: Vec<(i32, Tag)> = live.iter().map(|(_, slot, tag)| (*slot, *tag)).collect();
        let values = connection
            .frame_values(thread, jvm_frame.id, &slots)
            .map_err(|error| error.to_string())?;
        named.extend(live.into_iter().map(|(name, _, _)| name).zip(values));
        Ok(named)
    }

    /// What an object or array shown earlier opens into.
    pub fn children(&mut self, object: ObjectId) -> Result<Vec<Shown>, String> {
        let connection = self.shared.connection.clone();
        self.inspector
            .children(&connection, object)
            .map_err(|error| error.to_string())
    }

    /// The value of a path in a frame: a variable, `this`, or a field of
    /// `this` (of the continuation, in an async frame), then `.field`,
    /// `[index]`, `.size`/`.length` of an array.
    pub fn evaluate(
        &mut self,
        thread: ThreadId,
        frame: &Frame,
        expression: &str,
    ) -> Result<Shown, String> {
        let value = self.path_value(thread, frame, expression)?;
        let connection = self.shared.connection.clone();
        Ok(self.inspector.show(&connection, expression, value))
    }

    fn path_value(
        &mut self,
        thread: ThreadId,
        frame: &Frame,
        expression: &str,
    ) -> Result<Value, String> {
        let connection = self.shared.connection.clone();
        let mut steps = parse_path(expression)?.into_iter();
        let Some(PathStep::Name(first)) = steps.next() else {
            return Err("start with a name".to_owned());
        };
        let mut value = match frame.kind {
            FrameKind::Async { continuation } => self
                .inspector
                .field_value(&connection, continuation, &first)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("no `{first}` saved"))?,
            _ => {
                let named = self.named_values(thread, frame)?;
                match named.into_iter().find(|(name, _)| *name == first) {
                    Some((_, value)) => value,
                    None => self.field_of_this(thread, frame, &first)?,
                }
            }
        };
        for step in steps {
            value = self.follow(&connection, value, &step)?;
        }
        Ok(value)
    }

    fn field_of_this(
        &mut self,
        thread: ThreadId,
        frame: &Frame,
        name: &str,
    ) -> Result<Value, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let jvm_frame = frames.get(frame.index).ok_or("no such frame")?;
        let this = connection
            .this_object(thread, jvm_frame.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("no variable `{name}`"))?;
        self.inspector
            .field_value(&connection, this, name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("no variable or field `{name}`"))
    }

    fn follow(
        &mut self,
        connection: &Connection,
        value: Value,
        step: &PathStep,
    ) -> Result<Value, String> {
        let Value::Object { tag, id } = value else {
            return Err(format!(
                "{} has no members",
                self.inspector.show(connection, "", value).value
            ));
        };
        match step {
            PathStep::Name(name) if tag == Tag::Array && (name == "size" || name == "length") => {
                let length = connection
                    .array_length(id)
                    .map_err(|error| error.to_string())?;
                Ok(Value::Int(i32::try_from(length).unwrap_or(i32::MAX)))
            }
            PathStep::Name(name) => self
                .inspector
                .field_value(connection, id, name)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("no field `{name}`")),
            PathStep::Index(index) => {
                let elements = self
                    .inspector
                    .children(connection, id)
                    .map_err(|error| error.to_string())?;
                let target = format!("[{index}]");
                if !elements.iter().any(|element| element.name == target) {
                    return Err(format!("no element {target}"));
                }
                let element_values = if tag == Tag::Array {
                    connection
                        .array_values(id, *index, 1)
                        .map_err(|error| error.to_string())?
                } else {
                    let data = self
                        .inspector
                        .field_value(connection, id, "elementData")
                        .map_err(|error| error.to_string())?;
                    match data {
                        Some(Value::Object { id: array, .. }) => connection
                            .array_values(array, *index, 1)
                            .map_err(|error| error.to_string())?,
                        _ => {
                            return Err(format!(
                                "can't index into {}",
                                self.inspector.show(connection, "", value).value
                            ));
                        }
                    }
                };
                element_values
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("no element {target}"))
            }
        }
    }

    /// What the program printed (its last `count` lines), logpoints'
    /// messages among them.
    pub fn output_tail(&self, count: usize) -> Vec<String> {
        self.debuggee.output_tail(count)
    }

    pub fn has_ended(&self) -> bool {
        self.ended
    }

    /// Ends the program and the session.
    pub fn terminate(mut self) {
        if !self.ended {
            let _ = self.connection().exit(1);
        }
        self.debuggee.kill();
    }
}

/// `shop` of `shop.Order`.
fn package_of(class_name: &str) -> String {
    class_name
        .rsplit_once('.')
        .map_or("", |(package, _)| package)
        .to_owned()
}

enum PathStep {
    Name(String),
    Index(usize),
}

/// `order.items[0].price` as steps.
fn parse_path(expression: &str) -> Result<Vec<PathStep>, String> {
    let mut steps = Vec::new();
    let mut rest = expression.trim();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']').ok_or("an unclosed `[`")?;
            let index = after[..close]
                .trim()
                .parse()
                .map_err(|_| format!("`{}` isn't an index", &after[..close]))?;
            steps.push(PathStep::Index(index));
            rest = &after[close + 1..];
            continue;
        }
        let rest_after_dot = rest.strip_prefix('.').unwrap_or(rest);
        let end = rest_after_dot
            .find(['.', '['])
            .unwrap_or(rest_after_dot.len());
        let name = rest_after_dot[..end].trim().trim_matches('`');
        let is_name = !name.is_empty()
            && name
                .chars()
                .all(|char| char.is_alphanumeric() || char == '_' || char == '$');
        if !is_name {
            return Err(format!(
                "`{expression}` isn't a path like `order.items[0].price`"
            ));
        }
        steps.push(PathStep::Name(name.to_owned()));
        rest = &rest_after_dot[end..];
    }
    Ok(steps)
}

/// The line of a code index: the last line entry at or before it.
fn line_at(table: &jdwp::LineTable, index: u64) -> Option<i32> {
    table
        .lines
        .iter()
        .take_while(|(start, _)| *start <= index)
        .last()
        .map(|(_, line)| *line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_names_and_indices() {
        let steps = parse_path("order.items[2].`price`").unwrap();
        let shown: Vec<String> = steps
            .iter()
            .map(|step| match step {
                PathStep::Name(name) => name.clone(),
                PathStep::Index(index) => format!("[{index}]"),
            })
            .collect();
        assert_eq!(shown, ["order", "items", "[2]", "price"]);
        assert!(parse_path("order.total()").is_err());
        assert!(parse_path("items[x]").is_err());
    }

    #[test]
    fn a_code_index_is_on_the_last_line_starting_before_it() {
        let table = jdwp::LineTable {
            start: 0,
            end: 20,
            lines: vec![(0, 10), (4, 11), (9, 13)],
        };
        assert_eq!(line_at(&table, 0), Some(10));
        assert_eq!(line_at(&table, 7), Some(11));
        assert_eq!(line_at(&table, 15), Some(13));
    }
}
