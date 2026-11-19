//! Where events are written: standard error, a log file per session, and
//! (for tests) memory.

use std::io::Write;
use std::path::PathBuf;

use super::Level;
use super::event::Record;
use super::fields::Hashed;

/// How large a session's log file grows with everything; past it only
/// warnings and errors (and the recent events before them) are written, up
/// to `FILE_HARD_LIMIT`.
const FILE_LIMIT: u64 = 5 * 1024 * 1024;
const FILE_HARD_LIMIT: u64 = 6 * 1024 * 1024;

/// How many sessions' log files are kept per workspace.
const SESSIONS_KEPT: usize = 5;

#[derive(Default)]
pub(super) struct Sinks {
    pub(super) stderr: bool,
    pub(super) file: Option<FileSink>,
    /// What tests capture: every event, as JSON lines.
    pub(super) memory: Option<Vec<String>>,
}

impl Sinks {
    pub(super) fn write(&mut self, record: &Record) {
        self.write_one(record, false, true);
    }

    /// What the flight recorder kept back, written before a warning (to
    /// the file) or an error (to standard error too).
    pub(super) fn write_recent(&mut self, records: &[Record], level: Level) {
        let to_stderr = level == Level::Error;
        for record in records {
            self.write_one(record, true, to_stderr);
        }
    }

    fn write_one(&mut self, record: &Record, recent: bool, to_stderr: bool) {
        if self.stderr && to_stderr {
            // Not `eprintln!`: it panics when the editor has closed the
            // pipe, and a log mustn't take the server down.
            let _ = writeln!(std::io::stderr().lock(), "{}", record.to_human_line(recent));
        }
        if let Some(file) = &mut self.file {
            let wanted_past_limit = recent || record.event.level >= Level::Warn;
            file.write_line(&record.to_json_line(recent), wanted_past_limit);
        }
    }

    /// Every event, whatever its level, when captured (tests).
    pub(super) fn capture(&mut self, record: &Record) {
        if let Some(memory) = &mut self.memory {
            memory.push(record.to_json_line(false));
        }
    }
}

/// This session's log file: `<cache>/logs/<workspace hash>/<start>-<pid>.jsonl`.
pub(super) struct FileSink {
    file: std::fs::File,
    path: PathBuf,
    written: u64,
    full: bool,
}

impl FileSink {
    /// Opens a new file for the workspace hashed `workspace`, removing the
    /// oldest sessions' beyond the few kept; `None` when it can't be
    /// written.
    pub(super) fn open(workspace: Hashed) -> Option<FileSink> {
        let directory = crate::server::cache_directory()?
            .join("logs")
            .join(workspace.to_string());
        std::fs::create_dir_all(&directory).ok()?;
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let path = directory.join(format!("{started}-{}.jsonl", std::process::id()));
        let file = std::fs::File::create(&path).ok()?;
        remove_old_sessions(&directory);
        Some(FileSink {
            file,
            path,
            written: 0,
            full: false,
        })
    }

    /// Writes `line` unless the file is full: past `FILE_LIMIT` only a
    /// line `wanted_past_limit` (a warning, an error, what led to them).
    fn write_line(&mut self, line: &str, wanted_past_limit: bool) {
        let size = self.written + line.len() as u64;
        let limit = if wanted_past_limit {
            FILE_HARD_LIMIT
        } else {
            FILE_LIMIT
        };
        if size > limit {
            if !self.full {
                self.full = true;
                let _ = writeln!(self.file, "{{\"event\":\"log-full\"}}");
            }
            return;
        }
        if writeln!(self.file, "{line}").is_ok() {
            self.written += line.len() as u64 + 1;
        }
    }

    /// The file's path, the home directory written `~`.
    pub(super) fn display_path(&self) -> String {
        let path = self.path.to_string_lossy().into_owned();
        match std::env::var("HOME") {
            Ok(home) if !home.is_empty() && path.starts_with(&home) => {
                format!("~{}", &path[home.len()..])
            }
            _ => path,
        }
    }
}

fn remove_old_sessions(directory: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut sessions: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect();
    sessions.sort();
    let excess = sessions.len().saturating_sub(SESSIONS_KEPT);
    for old in sessions.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_the_limit_only_warnings_errors_and_what_led_to_them_are_written() {
        let path =
            std::env::temp_dir().join(format!("kotlin-analyzer-sink-{}", std::process::id()));
        let mut sink = FileSink {
            file: std::fs::File::create(&path).unwrap(),
            path: path.clone(),
            written: FILE_LIMIT - 10,
            full: false,
        };
        sink.write_line("{\"event\":\"response\"}", false);
        sink.write_line("{\"event\":\"panic\"}", true);
        sink.write_line("{\"event\":\"response\"}", false);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "{\"event\":\"log-full\"}\n{\"event\":\"panic\"}\n");
        std::fs::remove_file(&path).unwrap();
    }
}
