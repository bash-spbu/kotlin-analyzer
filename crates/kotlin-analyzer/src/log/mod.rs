//! The server's log: what it did, step by step, so that a problem met on a
//! codebase we can't see can be traced from the log alone — without the
//! log ever holding that codebase's code, names or paths.
//!
//! An event is a name and fields of safe kinds only (see `fields`): ids,
//! counts, durations, this program's own words, keyed hashes. At the
//! default level a session writes little: its start, each workspace load
//! and Gradle export, what was slow, what failed. Everything else — each
//! request, edit, diagnostics run — goes to the flight recorder, the last
//! events kept in memory, written out only when something goes wrong
//! (a slow request, a failure, a panic), right before the event that
//! tells what.
//!
//! Where it goes: standard error (the editor's server log), one line per
//! event; and a file of JSON lines per session under the cache directory
//! (`logs/<workspace hash>/`), the last few sessions kept — the file a
//! user sends with a report.

mod event;
mod fields;
pub(crate) mod key;
mod panics;
mod sink;
pub(crate) mod whois;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub(crate) use event::Event;
pub(crate) use fields::{FileRef, Hashed, SafeToken};
pub(crate) use panics::{Context, within};

/// How much is written as it happens.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Level {
    /// The flight recorder's: written only when something goes wrong.
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// `debug` or `info` (the client's setting, `KOTLIN_ANALYZER_LOG`).
    pub(crate) fn from_setting(setting: &str) -> Option<Level> {
        match setting.trim() {
            "debug" | "trace" => Some(Level::Debug),
            "info" => Some(Level::Info),
            _ => None,
        }
    }

    fn from_number(number: u8) -> Level {
        match number {
            0 => Level::Debug,
            2 => Level::Warn,
            3 => Level::Error,
            _ => Level::Info,
        }
    }
}

/// How many events the flight recorder keeps.
const RECORDER_CAPACITY: usize = 1000;

struct Logger {
    started: Instant,
    /// What's written as it happens (`Level` as a number).
    threshold: AtomicU8,
    sequence: AtomicU64,
    panics: AtomicU64,
    /// Whether events leave the process (standard error, the log file):
    /// only the server run on stdio says so — not the tests.
    output: AtomicBool,
    /// Whether tests capture every event (see `capture`).
    capturing: AtomicBool,
    recorder: Mutex<VecDeque<event::Record>>,
    sinks: Mutex<sink::Sinks>,
}

fn logger() -> &'static Logger {
    static LOGGER: OnceLock<Logger> = OnceLock::new();
    LOGGER.get_or_init(|| Logger {
        started: Instant::now(),
        threshold: AtomicU8::new(Level::Info as u8),
        sequence: AtomicU64::new(0),
        panics: AtomicU64::new(0),
        output: AtomicBool::new(false),
        capturing: AtomicBool::new(false),
        recorder: Mutex::new(VecDeque::with_capacity(RECORDER_CAPACITY)),
        sinks: Mutex::new(sink::Sinks::default()),
    })
}

/// The server on stdio: events go to standard error and a log file, and a
/// panic is logged (scrubbed) instead of printed.
pub(crate) fn enable_output() {
    logger().output.store(true, Ordering::SeqCst);
    panics::install_hook();
}

/// A session starts for the workspace at `root`: its log file is opened
/// (when output is enabled) and `level` set.
pub(crate) fn start_session(root: &Path, level: Level) {
    let logger = logger();
    logger.threshold.store(level as u8, Ordering::SeqCst);
    panics::install_hook();
    if logger.output.load(Ordering::SeqCst) {
        let mut sinks = lock(&logger.sinks);
        sinks.stderr = true;
        sinks.file = sink::FileSink::open(Hashed::of(&root.to_string_lossy()));
    }
}

pub(crate) fn set_level(level: Level) {
    logger().threshold.store(level as u8, Ordering::SeqCst);
}

/// Where this session's log file is, with the home directory as `~`.
pub(crate) fn file_location() -> Option<String> {
    lock(&logger().sinks)
        .file
        .as_ref()
        .map(|file| file.display_path())
}

pub(crate) fn panics_so_far() -> u64 {
    logger().panics.load(Ordering::SeqCst)
}

/// Records `event`: written now when at the threshold or above, else
/// kept in the flight recorder. A warning or an error first writes out
/// what the recorder kept.
pub(crate) fn emit(event: Event) {
    // A panic while an event is being written comes back here through the
    // panic hook, on a thread that may hold the sinks: it isn't logged.
    if EMITTING.with(|emitting| emitting.replace(true)) {
        return;
    }
    struct Emitted;
    impl Drop for Emitted {
        fn drop(&mut self) {
            EMITTING.with(|emitting| emitting.set(false));
        }
    }
    let _emitted = Emitted;
    let logger = logger();
    let threshold = Level::from_number(logger.threshold.load(Ordering::SeqCst));
    let record = event::Record {
        sequence: logger.sequence.fetch_add(1, Ordering::SeqCst),
        elapsed: logger.started.elapsed(),
        thread: std::thread::current()
            .name()
            .map(SafeToken::of)
            .unwrap_or_else(|| SafeToken::of("unnamed")),
        event,
    };
    let level = record.event.level;
    if level == Level::Error && record.event.name == "panic" {
        logger.panics.fetch_add(1, Ordering::SeqCst);
    }
    let written = level >= threshold;
    let recent: Vec<event::Record> = if level >= Level::Warn {
        lock(&logger.recorder).drain(..).collect()
    } else {
        Vec::new()
    };
    let capturing = logger.capturing.load(Ordering::SeqCst);
    // Most events are only recorded: the sinks aren't touched for them.
    if written || capturing || !recent.is_empty() {
        let mut sinks = lock(&logger.sinks);
        if capturing {
            sinks.capture(&record);
        }
        if !recent.is_empty() {
            sinks.write_recent(&recent, level);
        }
        if written {
            sinks.write(&record);
        }
    }
    if !written {
        let mut recorder = lock(&logger.recorder);
        if recorder.len() == RECORDER_CAPACITY {
            recorder.pop_front();
        }
        recorder.push_back(record);
    }
}

thread_local! {
    /// Whether this thread is writing an event.
    static EMITTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Every event from now on, whatever its level, is also kept in memory as
/// JSON lines (for tests: the most a session could write).
#[cfg(test)]
pub(crate) fn capture() {
    let logger = logger();
    lock(&logger.sinks).memory.get_or_insert_with(Vec::new);
    logger.capturing.store(true, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn captured() -> Vec<String> {
    lock(&logger().sinks).memory.clone().unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_written_while_another_is_being_written_is_dropped_not_deadlocked() {
        capture();
        EMITTING.with(|emitting| emitting.set(true));
        Event::info("written-while-writing").emit();
        EMITTING.with(|emitting| emitting.set(false));
        Event::info("written-after").emit();
        let captured = captured().join("\n");
        assert!(!captured.contains("written-while-writing"));
        assert!(captured.contains("written-after"));
    }
}
