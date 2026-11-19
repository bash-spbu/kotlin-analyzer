//! The VM's event sets, turned into what the session decides on. A class
//! of a breakpoint's file loading gets the breakpoint here, then runs on —
//! resumed once per set, as the VM suspended once for it; anything else
//! goes to the session, which resumes the VM or stops.

use std::sync::mpsc::{Receiver, Sender};

use jdwp::{Event, EventSet, MethodId, ReferenceTypeId, SuspendPolicy, ThreadId};

use crate::coroutines::is_suspension_marker;
use crate::{Shared, Stop, breakpoints, values};

/// What happened, for the session to decide on.
pub(crate) enum Notice {
    /// An exception or the end: always a stop.
    Stop(Stop),
    /// Breakpoint requests hit; `stepped` when a step ended there too.
    Breakpoint {
        thread: ThreadId,
        requests: Vec<i32>,
        stepped: bool,
    },
    Step {
        thread: ThreadId,
    },
    /// A method a method-exit request watches returned; `suspended` when
    /// it returned a coroutine's suspension marker.
    Exited {
        thread: ThreadId,
        request: i32,
        class: ReferenceTypeId,
        method: MethodId,
        suspended: bool,
    },
}

pub(crate) fn handle_events(
    shared: &Shared,
    events: &Receiver<EventSet>,
    notices: &Sender<Notice>,
) {
    let connection = &shared.connection;
    for set in events {
        let mut notice: Option<Notice> = None;
        let mut prepared_thread = None;
        let mut stepped_thread = None;
        let mut breakpoint_requests = Vec::new();
        let mut breakpoint_thread = None;
        for event in set.events {
            match event {
                Event::ClassPrepare {
                    thread,
                    class,
                    signature,
                    request,
                    ..
                } => {
                    if let Ok(mut table) = shared.breakpoints.lock() {
                        breakpoints::prepared(connection, &mut table, request, class, &signature);
                    }
                    prepared_thread = Some(thread);
                }
                Event::Breakpoint {
                    thread, request, ..
                } => {
                    breakpoint_requests.push(request);
                    breakpoint_thread = Some(thread);
                }
                Event::Step { thread, .. } => stepped_thread = Some(thread),
                Event::Exception {
                    thread,
                    exception,
                    catch_location,
                    ..
                } => {
                    notice = Some(Notice::Stop(Stop::Exception {
                        thread,
                        exception: describe_exception(connection, exception),
                        caught: catch_location.is_some(),
                    }));
                }
                Event::MethodExit {
                    thread,
                    request,
                    location,
                    value,
                } => {
                    notice.get_or_insert(Notice::Exited {
                        thread,
                        request,
                        class: location.class,
                        method: location.method,
                        suspended: is_suspension_marker(connection, &value),
                    });
                }
                Event::VmDeath | Event::Disconnected => {
                    notice = Some(Notice::Stop(Stop::Ended { exit_code: None }));
                }
                Event::VmStart { .. } | Event::ThreadStart { .. } | Event::ThreadDeath { .. } => {}
            }
        }
        let is_stop = matches!(notice, Some(Notice::Stop(_)));
        if !is_stop {
            if let Some(thread) = breakpoint_thread {
                notice = Some(Notice::Breakpoint {
                    thread,
                    requests: breakpoint_requests,
                    stepped: stepped_thread.is_some(),
                });
            } else if let Some(thread) = stepped_thread {
                notice = Some(Notice::Step { thread });
            }
        }
        match notice {
            Some(notice) => {
                let ended = matches!(notice, Notice::Stop(Stop::Ended { .. }));
                if notices.send(notice).is_err() || ended {
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

/// `IllegalStateException: no items` of an exception object.
fn describe_exception(connection: &jdwp::Connection, exception: jdwp::Value) -> String {
    let jdwp::Value::Object { id, .. } = exception else {
        return "an exception".to_owned();
    };
    let mut inspector = values::Inspector::default();
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
            jdwp::Value::Object { id, .. } => connection.string_value(id).ok(),
            _ => None,
        });
    match message {
        Some(message) => format!("{type_name}: {message}"),
        None => type_name,
    }
}
