//! Progress of work running off the main thread — loading, checking the
//! whole workspace — shown in the editor's status bar (`$/progress`). The
//! work reports; the main loop sends (`Task::Progress`), so a report never
//! overtakes the progress's beginning.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use lsp_types::{
    WorkDoneProgress, WorkDoneProgressBegin, WorkDoneProgressEnd, WorkDoneProgressReport,
};

use super::Task;

/// A progress's title as the status bar shows it: named after the server,
/// as an editor with several language servers shows theirs side by side
/// (Zed shows a title as it is).
pub(super) fn title(title: &str) -> String {
    format!("kotlin-analyzer: {title}")
}

/// A counter's reports more often than this are dropped: the status bar
/// can't show them anyway.
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Progress {
    tasks: Sender<Task>,
    token: String,
    /// The title to begin with — `None` when the main loop began it.
    title: Option<&'static str>,
    state: Mutex<State>,
}

struct State {
    started: Instant,
    /// Shown only once the work has gone on this long.
    delay: Duration,
    begun: bool,
    ended: bool,
    last_report: Option<Instant>,
}

impl Progress {
    /// A progress the main loop has begun: the work reports on it, the
    /// main loop ends it.
    pub(super) fn begun(tasks: Sender<Task>, token: String) -> Progress {
        Progress::new(tasks, token, None, Duration::ZERO, true)
    }

    /// A progress begun only if the work goes on for `delay` — quick work
    /// (rechecking after an edit) doesn't flash in the status bar — and
    /// ended when the work is done (or dropped).
    pub(super) fn deferred(
        tasks: Sender<Task>,
        token: String,
        title: &'static str,
        delay: Duration,
    ) -> Progress {
        Progress::new(tasks, token, Some(title), delay, false)
    }

    fn new(
        tasks: Sender<Task>,
        token: String,
        title: Option<&'static str>,
        delay: Duration,
        begun: bool,
    ) -> Progress {
        Progress {
            tasks,
            token,
            title,
            state: Mutex::new(State {
                started: Instant::now(),
                delay,
                begun,
                ended: false,
                last_report: None,
            }),
        }
    }

    /// What the work is doing now, and how far it is (0–100).
    pub(super) fn report(&self, message: String, percentage: Option<u32>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.ended || state.started.elapsed() < state.delay {
            return;
        }
        let now = Instant::now();
        if !state.begun {
            state.begun = true;
            state.last_report = Some(now);
            self.send(
                true,
                WorkDoneProgress::Begin(WorkDoneProgressBegin {
                    title: title(self.title.unwrap_or_default()),
                    cancellable: Some(false),
                    message: Some(message),
                    percentage,
                }),
            );
            return;
        }
        // A counter's updates are thinned out; a new step always shows.
        let is_count = percentage.is_some();
        let too_soon = is_count
            && state
                .last_report
                .is_some_and(|last| now.duration_since(last) < REPORT_INTERVAL);
        if too_soon {
            return;
        }
        state.last_report = Some(now);
        self.send(
            false,
            WorkDoneProgress::Report(WorkDoneProgressReport {
                cancellable: Some(false),
                message: Some(message),
                percentage,
            }),
        );
    }

    /// Ends the progress, if it was shown, with `message`.
    pub(super) fn end(&self, message: Option<String>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.ended || !state.begun || self.title.is_none() {
            state.ended = true;
            return;
        }
        state.ended = true;
        self.send(
            false,
            WorkDoneProgress::End(WorkDoneProgressEnd { message }),
        );
    }

    fn send(&self, create: bool, progress: WorkDoneProgress) {
        let _ = self.tasks.send(Task::Progress {
            token: self.token.clone(),
            create,
            progress,
        });
    }
}

impl Drop for Progress {
    /// Work that stopped early (cancelled, failed) leaves no progress
    /// spinning.
    fn drop(&mut self) {
        self.end(None);
    }
}

/// How far `done` of `total` is, in percent.
pub(super) fn percentage(done: usize, total: usize) -> Option<u32> {
    (total > 0).then(|| u32::try_from(done * 100 / total).unwrap_or(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(receiver: &crossbeam_channel::Receiver<Task>) -> Vec<String> {
        receiver
            .try_iter()
            .map(|task| match task {
                Task::Progress {
                    create, progress, ..
                } => {
                    let kind = match progress {
                        WorkDoneProgress::Begin(begin) => format!(
                            "begin {}: {}",
                            begin.title,
                            begin.message.unwrap_or_default()
                        ),
                        WorkDoneProgress::Report(report) => {
                            format!("report {}", report.message.unwrap_or_default())
                        }
                        WorkDoneProgress::End(end) => {
                            format!("end {}", end.message.unwrap_or_default())
                        }
                    };
                    if create {
                        format!("create, {kind}")
                    } else {
                        kind
                    }
                }
                _ => "other".to_owned(),
            })
            .collect()
    }

    #[test]
    fn deferred_progress_shows_only_work_that_lasts_and_ends_once() {
        let (tasks, receiver) = crossbeam_channel::unbounded();
        let quick = Progress::deferred(
            tasks.clone(),
            "quick".to_owned(),
            "Checking",
            Duration::from_secs(60),
        );
        quick.report("1/2".to_owned(), Some(50));
        drop(quick);
        assert!(sent(&receiver).is_empty(), "quick work shows nothing");

        let slow = Progress::deferred(tasks, "slow".to_owned(), "Checking", Duration::ZERO);
        slow.report("1/3 files".to_owned(), Some(33));
        // Too soon after the last: dropped.
        slow.report("2/3 files".to_owned(), Some(66));
        std::thread::sleep(REPORT_INTERVAL);
        slow.report("3/3 files".to_owned(), Some(100));
        slow.end(Some("done".to_owned()));
        drop(slow);
        assert_eq!(
            sent(&receiver),
            [
                "create, begin kotlin-analyzer: Checking: 1/3 files",
                "report 3/3 files",
                "end done"
            ]
        );
    }
}
