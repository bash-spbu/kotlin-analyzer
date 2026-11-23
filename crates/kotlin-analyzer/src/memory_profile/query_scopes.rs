//! Which query a thread is in, from the `fetch` span Salsa opens around
//! each query it is asked for (with its `detailed-trace` feature): a
//! `tracing` subscriber that hears only those spans and makes the query
//! the allocator's scope while one is entered.
//!
//! A span's id is its scope (plus one): entering it needs no lookup and
//! nothing is stored per span.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata};

use super::allocator::{self, SCOPES, ScopeId};

pub(super) struct QueryScopes;

/// Salsa's module that opens the span, and asks first whether a `DEBUG`
/// span would be heard there (`tracing::enabled!`, a hint).
const FETCH_MODULE: &str = "salsa::function::fetch";

fn is_query_fetch(metadata: &Metadata<'_>) -> bool {
    let in_fetch = metadata.target() == FETCH_MODULE;
    // A hint (`enabled!`) is neither a span nor an event.
    let is_span_or_hint = !metadata.is_event();
    in_fetch && is_span_or_hint
}

thread_local! {
    /// The scopes of the spans this thread entered, innermost last.
    static ENTERED: RefCell<Vec<ScopeId>> = const { RefCell::new(Vec::new()) };
    /// The scopes this thread has seen, so a span needs no lock.
    static KNOWN: RefCell<Vec<(Box<str>, ScopeId)>> = const { RefCell::new(Vec::new()) };
}

/// The queries' names; a name's scope is its place plus one.
static NAMES: Mutex<Vec<Box<str>>> = Mutex::new(Vec::new());

/// The scope of the query `name`.
pub(super) fn scope_of(name: &str) -> ScopeId {
    let known = KNOWN.with(|known| {
        known
            .borrow()
            .iter()
            .find(|(known_name, _)| **known_name == *name)
            .map(|(_, scope)| *scope)
    });
    if let Some(scope) = known {
        return scope;
    }
    let scope = register(name);
    KNOWN.with(|known| known.borrow_mut().push((name.into(), scope)));
    scope
}

fn register(name: &str) -> ScopeId {
    let mut names = NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let place = match names.iter().position(|known| **known == *name) {
        Some(place) => place,
        None => {
            names.push(name.into());
            names.len() - 1
        }
    };
    let last = SCOPES as u32 - 1;
    ScopeId((place as u32 + 1).min(last))
}

/// Every query that had a scope, with it.
pub(super) fn scopes() -> Vec<(String, ScopeId)> {
    let names = NAMES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    names
        .iter()
        .enumerate()
        .map(|(place, name)| (name.to_string(), ScopeId(place as u32 + 1)))
        .filter(|(_, scope)| (scope.0 as usize) < SCOPES)
        .collect()
}

/// A query's name as a span's `query` field shows it (`infer_body(Id(5))`),
/// up to the `(`, without allocating.
struct QueryName {
    bytes: [u8; 64],
    length: usize,
    complete: bool,
}

impl Default for QueryName {
    fn default() -> QueryName {
        QueryName {
            bytes: [0; 64],
            length: 0,
            complete: false,
        }
    }
}

impl QueryName {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).unwrap_or("?")
    }
}

impl std::fmt::Write for QueryName {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        for char in text.chars() {
            let fits = self.length + char.len_utf8() <= self.bytes.len();
            if self.complete || char == '(' || !fits {
                self.complete = true;
                return Ok(());
            }
            char.encode_utf8(&mut self.bytes[self.length..]);
            self.length += char.len_utf8();
        }
        Ok(())
    }
}

impl Visit for QueryName {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "query" {
            let _ = write!(self, "{value:?}");
        }
    }
}

impl tracing::Subscriber for QueryScopes {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if is_query_fetch(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        is_query_fetch(metadata)
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::DEBUG)
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        let mut name = QueryName::default();
        span.record(&mut name);
        let scope = scope_of(name.as_str());
        Id::from_u64(u64::from(scope.0) + 1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, span: &Id) {
        let scope = ScopeId((span.into_u64() - 1) as u32);
        let outer = allocator::current();
        ENTERED.with(|entered| entered.borrow_mut().push(outer));
        allocator::set_current(scope);
    }

    fn exit(&self, _span: &Id) {
        let outer = ENTERED.with(|entered| entered.borrow_mut().pop());
        allocator::set_current(outer.unwrap_or(ScopeId::OUTSIDE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_name_is_what_comes_before_its_key() {
        struct Key;
        impl std::fmt::Debug for Key {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "infer_body(Id({}))", 5)
            }
        }
        let mut name = QueryName::default();
        let _ = write!(name, "{:?}", Key);
        assert_eq!(name.as_str(), "infer_body");
        let mut long = QueryName::default();
        let _ = write!(long, "{}", "x".repeat(100));
        assert_eq!(long.as_str().len(), 64);
    }

    #[test]
    fn a_name_keeps_its_scope_on_every_thread() {
        let here = scope_of("test_query_scope");
        let there = std::thread::spawn(|| scope_of("test_query_scope"))
            .join()
            .unwrap();
        assert_eq!(here, there);
        assert_ne!(here, scope_of("another_test_query_scope"));
        assert!(
            scopes()
                .iter()
                .any(|(name, scope)| name == "test_query_scope" && *scope == here)
        );
    }
}
