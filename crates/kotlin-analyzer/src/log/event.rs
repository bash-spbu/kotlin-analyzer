//! An event: what happened, by name, with fields only of safe kinds — the
//! builder takes no other.

use std::time::Duration;

use super::Level;
use super::fields::{FileRef, Frame, Hashed, SafeToken, Scrubbed, SourceLocation, Value};

pub(crate) struct Event {
    /// What happened, in this program's words (`load-finished`).
    pub(crate) name: &'static str,
    pub(crate) level: Level,
    pub(crate) fields: Vec<(&'static str, Value)>,
}

impl Event {
    pub(crate) fn debug(name: &'static str) -> Event {
        Event::at(Level::Debug, name)
    }

    pub(crate) fn info(name: &'static str) -> Event {
        Event::at(Level::Info, name)
    }

    pub(crate) fn warn(name: &'static str) -> Event {
        Event::at(Level::Warn, name)
    }

    pub(crate) fn error(name: &'static str) -> Event {
        Event::at(Level::Error, name)
    }

    fn at(level: Level, name: &'static str) -> Event {
        Event {
            name,
            level,
            fields: Vec::new(),
        }
    }

    fn with(mut self, key: &'static str, value: Value) -> Event {
        self.fields.push((key, value));
        self
    }

    pub(crate) fn count(self, key: &'static str, count: impl TryInto<u64>) -> Event {
        let count = count.try_into().unwrap_or(u64::MAX);
        self.with(key, Value::Count(count))
    }

    pub(crate) fn number(self, key: &'static str, number: i64) -> Event {
        self.with(key, Value::Number(number))
    }

    pub(crate) fn duration(self, key: &'static str, duration: Duration) -> Event {
        self.with(key, Value::Duration(duration))
    }

    pub(crate) fn flag(self, key: &'static str, flag: bool) -> Event {
        self.with(key, Value::Flag(flag))
    }

    /// Words of this program's source: a method, a kind, an outcome.
    pub(crate) fn text(self, key: &'static str, text: &'static str) -> Event {
        self.with(key, Value::Static(text))
    }

    pub(crate) fn texts(self, key: &'static str, texts: Vec<&'static str>) -> Event {
        self.with(key, Value::Statics(texts))
    }

    pub(crate) fn file(self, key: &'static str, file: FileRef) -> Event {
        self.with(key, Value::File(file))
    }

    pub(crate) fn hash(self, key: &'static str, hash: Hashed) -> Event {
        self.with(key, Value::Hash(hash))
    }

    pub(crate) fn token(self, key: &'static str, token: SafeToken) -> Event {
        self.with(key, Value::Token(token))
    }

    pub(super) fn message(self, key: &'static str, message: Scrubbed) -> Event {
        self.with(key, Value::Scrubbed(message))
    }

    pub(super) fn source(self, key: &'static str, location: SourceLocation) -> Event {
        self.with(key, Value::Source(location))
    }

    pub(super) fn frames(self, key: &'static str, frames: Vec<Frame>) -> Event {
        self.with(key, Value::Frames(frames))
    }

    /// Writes the event through `log::emit`.
    pub(crate) fn emit(self) {
        super::emit(self);
    }
}

/// An event as it happened: when, in what order, on which thread.
pub(super) struct Record {
    pub(super) sequence: u64,
    pub(super) elapsed: Duration,
    pub(super) thread: SafeToken,
    pub(super) event: Event,
}

impl Record {
    /// One JSON line; `recent` marks one the flight recorder kept back.
    pub(super) fn to_json_line(&self, recent: bool) -> String {
        let mut object = serde_json::Map::new();
        object.insert(
            "t".to_owned(),
            serde_json::json!((self.elapsed.as_secs_f64() * 1000.0).round() / 1000.0),
        );
        object.insert("seq".to_owned(), serde_json::json!(self.sequence));
        object.insert(
            "level".to_owned(),
            serde_json::json!(self.event.level.name()),
        );
        object.insert("event".to_owned(), serde_json::json!(self.event.name));
        object.insert(
            "thread".to_owned(),
            Value::Token(self.thread.clone()).to_json(),
        );
        if recent {
            object.insert("recent".to_owned(), serde_json::json!(true));
        }
        for (key, value) in &self.event.fields {
            object.insert((*key).to_owned(), value.to_json());
        }
        serde_json::Value::Object(object).to_string()
    }

    /// One line for a reader: time, level, event, fields.
    pub(super) fn to_human_line(&self, recent: bool) -> String {
        let mut line = format!(
            "{:>9.3} {:<5} {}{}",
            self.elapsed.as_secs_f64(),
            self.event.level.name(),
            if recent { "(recent) " } else { "" },
            self.event.name
        );
        for (key, value) in &self.event.fields {
            line.push(' ');
            line.push_str(key);
            line.push('=');
            line.push_str(&value.to_human());
        }
        line
    }
}
