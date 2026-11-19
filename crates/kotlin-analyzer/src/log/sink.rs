//! Where events are written: standard error, a log file per session, and
//! (for tests) memory.

use std::io::Write;
use std::path::PathBuf;

use super::Level;
use super::event::Record;
use super::fields::Hashed;

/// How large a session's log file may grow.
const FILE_LIMIT: u64 = 5 * 1024 * 1024;

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
            eprintln!("{}", record.to_human_line(recent));
        }
        if let Some(file) = &mut self.file {
            file.write_line(&record.to_json_line(recent));
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

    fn write_line(&mut self, line: &str) {
        if self.full {
            return;
        }
        if self.written + line.len() as u64 > FILE_LIMIT {
            self.full = true;
            let _ = writeln!(self.file, "{{\"event\":\"log-full\"}}");
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
