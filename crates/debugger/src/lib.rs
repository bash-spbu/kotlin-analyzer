//! A JVM debugger for Kotlin and Java, over JDWP: what the DAP adapter
//! and the agents' MCP tools drive.
//!
//! A [`Session`] starts or reaches the JVM (`launch`), sets breakpoints by
//! file and line — a file's classes are found by its package and the
//! source file name each class records (`sources`), those loaded later
//! when they are — and, once the JVM stops, tells where (frames mapped
//! back to the project's files), shows variables (`values`) and evaluates
//! paths (`order.items[0].price`). Every stop suspends the whole VM.

pub mod launch;
pub mod sources;
pub mod values;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jdwp::{
    Connection, Event, EventKind, EventSet, Location, MethodId, Modifier, ObjectId,
    ReferenceTypeId, StepDepth, SuspendPolicy, Tag, ThreadId, Value,
};

pub use launch::Launch;
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

/// A breakpoint as set: where it is asked for, and where the JVM has it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Breakpoint {
    pub id: u32,
    pub path: PathBuf,
    pub line: u32,
    /// Set in a loaded class: the JVM will stop there. Otherwise it waits
    /// for a class of the file to load.
    pub verified: bool,
}

/// A frame of a stopped thread's stack.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub index: usize,
    /// `shop.Order`.
    pub class_name: String,
    pub method: String,
    /// The project's file it is in, when found.
    pub path: Option<PathBuf>,
    /// `Order.kt`, from the class.
    pub source_name: Option<String>,
    pub line: Option<i32>,
}

/// What the event thread and the session share.
struct Shared {
    connection: Arc<Connection>,
    breakpoints: Mutex<BreakpointTable>,
}

#[derive(Default)]
struct BreakpointTable {
    next_id: u32,
    entries: Vec<BreakpointEntry>,
}

struct BreakpointEntry {
    breakpoint: Breakpoint,
    key: SourceKey,
    /// The class-prepare request watching for the file's classes.
    prepare_request: Option<i32>,
    /// The JVM's breakpoint requests, by location.
    requests: HashMap<Location, i32>,
}

pub struct Session {
    shared: Arc<Shared>,
    debuggee: launch::Debuggee,
    stops: Receiver<Stop>,
    sources: SourceIndex,
    inspector: Inspector,
    exception_request: Option<i32>,
    /// A step begun and not ended yet: cleared at the next stop.
    step_request: Option<i32>,
    /// The thread of the latest stop.
    current_thread: Option<ThreadId>,
    ended: bool,
    methods: HashMap<ReferenceTypeId, Vec<jdwp::Method>>,
}

impl Session {
    /// Starts or reaches the JVM; returns once it has connected, still
    /// suspended — set breakpoints, then [`Session::resume`]. Frames map
    /// to files under `source_roots`.
    pub fn start(launch: &Launch, source_roots: &[PathBuf]) -> Result<Session, String> {
        Session::start_streaming(launch, source_roots, None)
    }

    /// [`Session::start`], each line the program prints sent to `lines`
    /// as it comes (a debug console's output).
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
        let (stop_sender, stops) = std::sync::mpsc::channel();
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("debugger-events".to_owned())
                .spawn(move || handle_events(&shared, &events, &stop_sender))
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
            stops,
            sources: SourceIndex::of(source_roots),
            inspector: Inspector::default(),
            exception_request: None,
            step_request: None,
            current_thread: None,
            ended: false,
            methods: HashMap::new(),
        })
    }

    fn connection(&self) -> &Connection {
        &self.shared.connection
    }

    /// Stops at `line` of `path`: in its classes loaded now, and in those
    /// that load later.
    pub fn set_breakpoint(&mut self, path: &Path, line: u32) -> Result<Breakpoint, String> {
        let key = SourceKey::of(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let connection = self.shared.connection.clone();
        let prepare_request = connection
            .set_event_request(
                EventKind::ClassPrepare,
                SuspendPolicy::EventThread,
                &[
                    Modifier::ClassMatch(key.class_pattern()),
                    Modifier::SourceNameMatch(key.file_name.clone()),
                ],
            )
            .ok();
        let mut requests = HashMap::new();
        for class in loaded_classes_of(&connection, &key) {
            for location in locations_at(&connection, class, line) {
                if let Ok(request) = set_breakpoint_request(&connection, &location) {
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
        };
        table.entries.push(BreakpointEntry {
            breakpoint: breakpoint.clone(),
            key,
            prepare_request,
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
        if let Some(request) = entry.prepare_request {
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
    /// `patience`.
    pub fn wait_for_stop(&mut self, patience: Duration) -> Option<Stop> {
        let stop = match self.stops.recv_timeout(patience) {
            Ok(stop) => stop,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => Stop::Ended { exit_code: None },
        };
        let stop = match stop {
            Stop::Ended { .. } => {
                self.ended = true;
                Stop::Ended {
                    exit_code: self.debuggee.wait(Duration::from_secs(60)),
                }
            }
            stopped => stopped,
        };
        if let Some(request) = self.step_request.take() {
            let _ = self
                .connection()
                .clear_event_request(EventKind::SingleStep, request);
        }
        self.current_thread = stop.thread();
        // A thread an event stopped can run `toString()`.
        self.inspector = Inspector::calling_in(stop.thread());
        Some(stop)
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
    /// [`Session::wait_for_stop`]) ends it.
    pub fn begin_step(&mut self, thread: ThreadId, depth: StepDepth) -> Result<(), String> {
        let mut modifiers = vec![Modifier::Step { thread, depth }, Modifier::Count(1)];
        modifiers.extend(
            STEP_EXCLUDED
                .iter()
                .map(|pattern| Modifier::ClassExclude((*pattern).to_owned())),
        );
        let request = self
            .connection()
            .set_event_request(EventKind::SingleStep, SuspendPolicy::All, &modifiers)
            .map_err(|error| error.to_string())?;
        self.step_request = Some(request);
        self.resume()
    }

    /// Suspends the running JVM.
    pub fn pause(&mut self) -> Result<Stop, String> {
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

    /// The stopped thread's frames, the innermost first.
    pub fn stack(&mut self, thread: ThreadId) -> Result<Vec<Frame>, String> {
        let frames = self
            .connection()
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let mut shown = Vec::with_capacity(frames.len());
        for (index, frame) in frames.iter().enumerate() {
            shown.push(self.frame_at(index, &frame.location));
        }
        Ok(shown)
    }

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
        let line = connection
            .line_table(location.class, location.method)
            .ok()
            .and_then(|table| line_at(&table, location.index));
        let package = class_name
            .rsplit_once('.')
            .map_or("", |(package, _)| package)
            .to_owned();
        let path = source_name
            .as_deref()
            .and_then(|name| self.sources.find(name, &package));
        Frame {
            index,
            class_name,
            method,
            path,
            source_name,
            line,
        }
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

    /// A frame's `this` and its live local variables and parameters.
    pub fn variables(
        &mut self,
        thread: ThreadId,
        frame_index: usize,
    ) -> Result<Vec<Shown>, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let frame = frames.get(frame_index).ok_or("no such frame")?;
        let mut shown = Vec::new();
        if let Ok(Some(this)) = connection.this_object(thread, frame.id) {
            let value = Value::Object {
                tag: Tag::Object,
                id: this,
            };
            shown.push(self.inspector.show(&connection, "this", value));
        }
        let location = frame.location;
        let variables = match connection.variable_table(location.class, location.method) {
            Ok(variables) => variables,
            Err(error) if error.is_absent_information() => return Ok(shown),
            Err(error) => return Err(error.to_string()),
        };
        let live: Vec<jdwp::Variable> = variables
            .into_iter()
            .filter(|variable| variable.is_live_at(location.index))
            // Kotlin's markers of inlined code, not the program's; `this`
            // is shown first already.
            .filter(|variable| {
                let is_marker = variable.name.starts_with("$i$") || variable.name.is_empty();
                !is_marker && variable.name != "this"
            })
            .collect();
        let slots: Vec<(i32, Tag)> = live
            .iter()
            .map(|variable| (variable.slot, Tag::of_signature(&variable.signature)))
            .collect();
        let values = connection
            .frame_values(thread, frame.id, &slots)
            .map_err(|error| error.to_string())?;
        for (variable, value) in live.iter().zip(values) {
            shown.push(self.inspector.show(&connection, &variable.name, value));
        }
        Ok(shown)
    }

    /// What an object or array shown earlier opens into.
    pub fn children(&mut self, object: ObjectId) -> Result<Vec<Shown>, String> {
        let connection = self.shared.connection.clone();
        self.inspector
            .children(&connection, object)
            .map_err(|error| error.to_string())
    }

    /// The value of a path in a frame: a variable, `this`, or a field of
    /// `this`, then `.field`, `[index]`, `.size`/`.length` of an array.
    pub fn evaluate(
        &mut self,
        thread: ThreadId,
        frame_index: usize,
        expression: &str,
    ) -> Result<Shown, String> {
        let connection = self.shared.connection.clone();
        let steps = parse_path(expression)?;
        let mut steps = steps.into_iter();
        let Some(PathStep::Name(first)) = steps.next() else {
            return Err("start with a name".to_owned());
        };
        let variables = self.variables(thread, frame_index)?;
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let frame = frames.get(frame_index).ok_or("no such frame")?;
        let mut value = match variables.iter().position(|variable| variable.name == first) {
            Some(_) => self.variable_value(thread, frame_index, &first)?,
            None => {
                let this = connection
                    .this_object(thread, frame.id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("no variable `{first}`"))?;
                self.inspector
                    .field_value(&connection, this, &first)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("no variable or field `{first}`"))?
            }
        };
        for step in steps {
            value = self.follow(&connection, value, &step)?;
        }
        Ok(self.inspector.show(&connection, expression, value))
    }

    /// The raw value of the frame's variable `name` (or `this`).
    fn variable_value(
        &mut self,
        thread: ThreadId,
        frame_index: usize,
        name: &str,
    ) -> Result<Value, String> {
        let connection = self.shared.connection.clone();
        let frames = connection
            .frames(thread)
            .map_err(|error| error.to_string())?;
        let frame = frames.get(frame_index).ok_or("no such frame")?;
        if name == "this" {
            return match connection.this_object(thread, frame.id) {
                Ok(Some(this)) => Ok(Value::Object {
                    tag: Tag::Object,
                    id: this,
                }),
                _ => Err("no `this` in a static method".to_owned()),
            };
        }
        let location = frame.location;
        let variables = connection
            .variable_table(location.class, location.method)
            .map_err(|error| error.to_string())?;
        let variable = variables
            .iter()
            .filter(|variable| variable.is_live_at(location.index))
            .find(|variable| variable.name == name)
            .ok_or_else(|| format!("no variable `{name}`"))?;
        let values = connection
            .frame_values(
                thread,
                frame.id,
                &[(variable.slot, Tag::of_signature(&variable.signature))],
            )
            .map_err(|error| error.to_string())?;
        values
            .into_iter()
            .next()
            .ok_or_else(|| "no value".to_owned())
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

    /// What the program printed (its last `count` lines).
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

/// The loaded classes of a source file: in its package, compiled from a
/// file of its name.
fn loaded_classes_of(connection: &Connection, key: &SourceKey) -> Vec<ReferenceTypeId> {
    let Ok(classes) = connection.all_classes() else {
        return Vec::new();
    };
    classes
        .into_iter()
        .filter(|class| key.holds_class(&class.signature))
        .filter(|class| {
            connection
                .source_file(class.id)
                .is_ok_and(|name| name == key.file_name)
        })
        .map(|class| class.id)
        .collect()
}

/// Where in `class` the code of `line` starts: in each method with the
/// line, its first code index.
fn locations_at(connection: &Connection, class: ReferenceTypeId, line: u32) -> Vec<Location> {
    let Ok(methods) = connection.methods(class) else {
        return Vec::new();
    };
    let mut locations = Vec::new();
    for method in methods {
        let Ok(table) = connection.line_table(class, method.id) else {
            continue;
        };
        let first = table
            .lines
            .iter()
            .filter(|(_, entry_line)| i64::from(*entry_line) == i64::from(line))
            .map(|(index, _)| *index)
            .min();
        if let Some(index) = first {
            locations.push(Location {
                type_tag: jdwp::TypeTag::Class,
                class,
                method: method.id,
                index,
            });
        }
    }
    locations
}

fn set_breakpoint_request(
    connection: &Connection,
    location: &Location,
) -> Result<i32, jdwp::Error> {
    connection.set_event_request(
        EventKind::Breakpoint,
        SuspendPolicy::All,
        &[Modifier::LocationOnly(*location)],
    )
}

/// The VM's event sets: a class of a breakpoint's file loading gets the
/// breakpoint, then runs on — resumed once per set, as the VM suspended
/// once for it; a set that stops goes to the session as one stop.
fn handle_events(
    shared: &Shared,
    events: &Receiver<EventSet>,
    stops: &std::sync::mpsc::Sender<Stop>,
) {
    let connection = &shared.connection;
    for set in events {
        let mut stop: Option<Stop> = None;
        let mut prepared_thread = None;
        for event in set.events {
            match event {
                Event::ClassPrepare {
                    thread,
                    class,
                    signature,
                    request,
                    ..
                } => {
                    prepared(shared, request, class, &signature);
                    prepared_thread = Some(thread);
                }
                Event::Breakpoint {
                    thread, request, ..
                } => {
                    let hit = breakpoints_of(shared, request);
                    stop = Some(match stop {
                        Some(Stop::Breakpoint {
                            mut breakpoints, ..
                        }) => {
                            breakpoints.extend(hit);
                            Stop::Breakpoint {
                                thread,
                                breakpoints,
                            }
                        }
                        _ => Stop::Breakpoint {
                            thread,
                            breakpoints: hit,
                        },
                    });
                }
                Event::Step { thread, .. } => {
                    // A breakpoint at the step's end says more.
                    if stop.is_none() {
                        stop = Some(Stop::Step { thread });
                    }
                }
                Event::Exception {
                    thread,
                    exception,
                    catch_location,
                    ..
                } => {
                    stop = Some(Stop::Exception {
                        thread,
                        exception: describe_exception(connection, exception),
                        caught: catch_location.is_some(),
                    });
                }
                Event::VmDeath | Event::Disconnected => {
                    stop = Some(Stop::Ended { exit_code: None })
                }
                Event::VmStart { .. } | Event::ThreadStart { .. } | Event::ThreadDeath { .. } => {}
            }
        }
        match stop {
            Some(stop) => {
                let ended = matches!(stop, Stop::Ended { .. });
                if stops.send(stop).is_err() || ended {
                    return;
                }
            }
            // Only classes loaded: run on, resuming what the set suspended.
            None => match (set.suspend_policy, prepared_thread) {
                (SuspendPolicy::EventThread, Some(thread)) => {
                    let _ = connection.resume_thread(thread);
                }
                (SuspendPolicy::All, Some(_)) => {
                    let _ = connection.resume_all();
                }
                _ => {}
            },
        }
    }
}

/// A class of a breakpoint's file loaded (for the class-prepare request
/// `request`): the breakpoint goes into it.
fn prepared(shared: &Shared, request: i32, class: ReferenceTypeId, signature: &str) {
    let connection = &shared.connection;
    let Ok(mut table) = shared.breakpoints.lock() else {
        return;
    };
    for entry in table.entries.iter_mut() {
        let watches = entry.prepare_request == Some(request) && entry.key.holds_class(signature);
        if !watches {
            continue;
        }
        for location in locations_at(connection, class, entry.breakpoint.line) {
            if entry.requests.contains_key(&location) {
                continue;
            }
            if let Ok(id) = set_breakpoint_request(connection, &location) {
                entry.requests.insert(location, id);
                entry.breakpoint.verified = true;
            }
        }
    }
}

/// The breakpoints the JVM's breakpoint request `request` is one of.
fn breakpoints_of(shared: &Shared, request: i32) -> Vec<u32> {
    shared
        .breakpoints
        .lock()
        .map(|table| {
            table
                .entries
                .iter()
                .filter(|entry| entry.requests.values().any(|id| *id == request))
                .map(|entry| entry.breakpoint.id)
                .collect()
        })
        .unwrap_or_default()
}

/// `IllegalStateException: no items` of an exception object.
fn describe_exception(connection: &Connection, exception: Value) -> String {
    let Value::Object { id, .. } = exception else {
        return "an exception".to_owned();
    };
    let mut inspector = Inspector::default();
    let type_name = connection
        .reference_type(id)
        .and_then(|class| inspector.signature(connection, class))
        .map(|signature| values::type_name(&signature))
        .unwrap_or_default();
    let message = inspector
        .field_value(connection, id, "detailMessage")
        .ok()
        .flatten()
        .and_then(|message| match message {
            Value::Object { id, .. } => connection.string_value(id).ok(),
            _ => None,
        });
    match message {
        Some(message) => format!("{type_name}: {message}"),
        None => type_name,
    }
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
