//! A panic, logged with what its thread was doing (the request, the file
//! it was about) and where in this program's code it happened — its
//! message with what it quotes cut out. A salsa cancellation unwinds too,
//! on purpose: it's no panic to log.

use std::backtrace::Backtrace;
use std::cell::RefCell;
use std::sync::Once;

use super::event::Event;
use super::fields::{FileRef, Scrubbed, SourceLocation, own_frames};

/// What a thread is doing, for a panic's log.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Context {
    Request {
        id: i64,
        method: &'static str,
        file: Option<FileRef>,
    },
    Diagnostics {
        file: FileRef,
        version: i32,
    },
    Loading {
        generation: u64,
    },
    GradleExport,
    JdkDownload,
}

thread_local! {
    static CONTEXT: RefCell<Vec<Context>> = const { RefCell::new(Vec::new()) };
}

/// Runs `run` with this thread doing `context`.
pub(crate) fn within<T>(context: Context, run: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            CONTEXT.with(|context| context.borrow_mut().pop());
        }
    }
    CONTEXT.with(|current| current.borrow_mut().push(context));
    let _leave = Leave;
    run()
}

fn current_context() -> Option<Context> {
    CONTEXT.with(|context| context.borrow().last().copied())
}

/// Logs every panic from now on. Where events don't leave the process
/// (the tests), the panic is also printed as before.
pub(super) fn install_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().is::<ide::Cancelled>() {
                return;
            }
            log_panic(info);
            let printed_already = super::logger()
                .output
                .load(std::sync::atomic::Ordering::SeqCst);
            if !printed_already {
                previous(info);
            }
        }));
    });
}

fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = match info.payload().downcast_ref::<&'static str>() {
        Some(literal) => Scrubbed::literal(literal),
        None => Scrubbed::of(
            info.payload()
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or("(not text)"),
        ),
    };
    let mut event = Event::error("panic").message("message", message);
    if let Some(location) = info.location() {
        event = event.source(
            "at",
            SourceLocation::of(location.file(), location.line(), location.column()),
        );
    }
    event = match current_context() {
        Some(Context::Request { id, method, file }) => {
            let event = event
                .text("while", "request")
                .number("request", id)
                .text("method", method);
            match file {
                Some(file) => event.file("file", file),
                None => event,
            }
        }
        Some(Context::Diagnostics { file, version }) => event
            .text("while", "diagnostics")
            .file("file", file)
            .number("version", i64::from(version)),
        Some(Context::Loading { generation }) => event
            .text("while", "loading")
            .count("generation", generation),
        Some(Context::GradleExport) => event.text("while", "gradle-export"),
        Some(Context::JdkDownload) => event.text("while", "jdk-download"),
        None => event,
    };
    let frames = own_frames(&Backtrace::force_capture().to_string(), 40);
    event.frames("stack", frames).emit();
}
