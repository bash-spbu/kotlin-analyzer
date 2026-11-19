//! What a log event may carry. Nothing here can be built from the user's
//! text as it is: a file is its id and a keyed hash of its path, a name
//! from outside is filtered to a short token, a panic's message has what it
//! quotes cut out. An event's fields are [`Value`]s, and a `Value` comes
//! only from these — so source code, identifiers and paths can't reach a
//! log by accident: there is no `Value` to put them in.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

/// A field of an event.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Count(u64),
    Number(i64),
    Duration(Duration),
    Flag(bool),
    /// Text written in this program's source: a method name, a kind.
    Static(&'static str),
    Statics(Vec<&'static str>),
    File(FileRef),
    Hash(Hashed),
    Token(SafeToken),
    Scrubbed(Scrubbed),
    Source(SourceLocation),
    Frames(Vec<Frame>),
}

impl Value {
    pub(super) fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Count(count) => json!(count),
            Value::Number(number) => json!(number),
            Value::Duration(duration) => json!(milliseconds(*duration)),
            Value::Flag(flag) => json!(flag),
            Value::Static(text) => json!(text),
            Value::Statics(texts) => json!(texts),
            Value::File(file) => json!({ "id": file.id, "path": file.path.to_string() }),
            Value::Hash(hash) => json!(hash.to_string()),
            Value::Token(token) => json!(token.0),
            Value::Scrubbed(scrubbed) => json!(scrubbed.0),
            Value::Source(location) => json!(location.0),
            Value::Frames(frames) => json!(
                frames
                    .iter()
                    .map(|frame| format!("{} {}", frame.function, frame.location))
                    .collect::<Vec<_>>()
            ),
        }
    }

    pub(super) fn to_human(&self) -> String {
        match self {
            Value::Count(count) => count.to_string(),
            Value::Number(number) => number.to_string(),
            Value::Duration(duration) => format!("{:.1}ms", milliseconds(*duration)),
            Value::Flag(flag) => flag.to_string(),
            Value::Static(text) => (*text).to_owned(),
            Value::Statics(texts) => texts.join(","),
            Value::File(file) => format!("file#{}:{}", file.id, file.path),
            Value::Hash(hash) => hash.to_string(),
            Value::Token(token) => token.0.clone(),
            Value::Scrubbed(scrubbed) => format!("{:?}", scrubbed.0),
            Value::Source(location) => location.0.clone(),
            Value::Frames(frames) => frames
                .iter()
                .map(|frame| format!("\n    {} {}", frame.function, frame.location))
                .collect(),
        }
    }
}

fn milliseconds(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 10_000.0).round() / 10.0
}

/// A workspace file: the server's id for it this session and a keyed hash
/// of its path (relative to the workspace root). `kotlin-analyzer
/// log-whois` tells the user — and only them, who hold the key — which
/// file it is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FileRef {
    pub(crate) id: u32,
    pub(crate) path: Hashed,
}

impl FileRef {
    pub(crate) fn new(id: u32, path: &Path, root: &Path) -> FileRef {
        FileRef {
            id,
            path: Hashed::path(path, root),
        }
    }
}

/// A keyed hash (SipHash with the user's own key, see `key`): the same
/// text gives the same hash in every log of this machine, and without the
/// key nothing tells what it was.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Hashed(u64);

impl Hashed {
    pub(crate) fn of(text: &str) -> Hashed {
        Hashed::with_key(super::key::current(), text)
    }

    pub(crate) fn with_key(key: (u64, u64), text: &str) -> Hashed {
        #[allow(deprecated)]
        let mut hasher = std::hash::SipHasher::new_with_keys(key.0, key.1);
        std::hash::Hasher::write(&mut hasher, text.as_bytes());
        Hashed(std::hash::Hasher::finish(&hasher) >> 16)
    }

    /// `path` as the workspace knows it: relative to `root` when inside.
    pub(crate) fn path(path: &Path, root: &Path) -> Hashed {
        let relative = path.strip_prefix(root).unwrap_or(path);
        Hashed::of(&relative.to_string_lossy())
    }
}

impl std::fmt::Display for Hashed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:012x}", self.0)
    }
}

/// A short name from outside the program (the client's name and version):
/// letters, digits and `._-+ ` only, at most 40 of them — else `other`.
#[derive(Clone, Debug)]
pub(crate) struct SafeToken(String);

impl SafeToken {
    pub(crate) fn of(text: &str) -> SafeToken {
        let plain = text.len() <= 40
            && text
                .chars()
                .all(|char| char.is_ascii_alphanumeric() || "._-+ ".contains(char));
        SafeToken(if plain {
            text.to_owned()
        } else {
            "other".to_owned()
        })
    }
}

/// A message this program formatted (a panic's): with what it quotes, and
/// anything that looks like a path, cut out — those are where a value from
/// the user's code would be.
#[derive(Clone, Debug)]
pub(crate) struct Scrubbed(String);

impl Scrubbed {
    pub(crate) fn of(message: &str) -> Scrubbed {
        let mut scrubbed = String::new();
        let mut quote: Option<char> = None;
        let mut previous: Option<char> = None;
        for char in message.chars() {
            // `can't`: an apostrophe inside a word quotes nothing.
            let apostrophe = char == '\'' && previous.is_some_and(char::is_alphanumeric);
            match quote {
                Some(open) if char == open => {
                    scrubbed.push('…');
                    scrubbed.push(char);
                    quote = None;
                }
                Some(_) => {}
                None if matches!(char, '"' | '\'' | '`') && !apostrophe => {
                    scrubbed.push(char);
                    quote = Some(char);
                }
                None => scrubbed.push(char),
            }
            previous = Some(char);
        }
        let words: Vec<String> = scrubbed
            .split(' ')
            .map(|word| {
                let looks_like_path = word.contains('/') || word.contains('\\');
                if looks_like_path {
                    "<path>".to_owned()
                } else {
                    word.to_owned()
                }
            })
            .collect();
        let mut joined = words.join(" ");
        if joined.chars().count() > 300 {
            joined = joined.chars().take(300).collect::<String>() + "…";
        }
        Scrubbed(joined)
    }

    /// A message written in this program's source as it is.
    pub(crate) fn literal(message: &'static str) -> Scrubbed {
        Scrubbed(message.to_owned())
    }
}

/// A place in this program's source (where a panic happened).
#[derive(Clone, Debug)]
pub(crate) struct SourceLocation(String);

impl SourceLocation {
    pub(crate) fn of(file: &str, line: u32, column: u32) -> SourceLocation {
        SourceLocation(format!("{}:{line}:{column}", source_location(file)))
    }
}

/// A frame of a stack trace in this program's own code: its function and
/// where in the program's source it is.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub(crate) function: String,
    pub(crate) location: String,
}

/// The frames of `backtrace` (as `std::backtrace` writes it) in this
/// program's own source — under its `crates/`, not a dependency's or the
/// standard library's — innermost first, at most `limit`.
pub(crate) fn own_frames(backtrace: &str, limit: usize) -> Vec<Frame> {
    let lines: Vec<&str> = backtrace.lines().collect();
    let mut frames = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((number, function)) = line.trim_start().split_once(": ") else {
            continue;
        };
        if number.parse::<u32>().is_err() {
            continue;
        }
        let Some(location) = lines
            .get(index + 1)
            .and_then(|next| next.trim_start().strip_prefix("at "))
        else {
            continue;
        };
        let own = location.contains("crates/") && !location.contains("/registry/");
        if !own {
            continue;
        }
        frames.push(Frame {
            function: function.to_owned(),
            location: source_location(location),
        });
        if frames.len() == limit {
            break;
        }
    }
    frames
}

/// A location in this program's source, from `crates/` on (whatever the
/// build machine's directory was).
pub(crate) fn source_location(location: &str) -> String {
    match location.find("crates/") {
        Some(start) => location[start..].to_owned(),
        None => Path::new(location.split(':').next().unwrap_or(""))
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scrubbed_message_keeps_its_words_but_not_what_it_quotes_or_paths() {
        let scrubbed = Scrubbed::of(
            "no item `secretName` in \"Payroll.kt\" at /Users/someone/work/Payroll.kt: index 5",
        );
        assert_eq!(scrubbed.0, "no item `…` in \"…\" at <path> index 5");
    }

    #[test]
    fn an_apostrophe_in_a_word_quotes_nothing() {
        let scrubbed = Scrubbed::of("the index can't be 5 for 'secret' here, it isn't known");
        assert_eq!(
            scrubbed.0,
            "the index can't be 5 for '…' here, it isn't known"
        );
    }

    #[test]
    fn a_token_from_outside_is_kept_only_when_plain_and_short() {
        assert_eq!(SafeToken::of("Zed 0.190.1").0, "Zed 0.190.1");
        assert_eq!(SafeToken::of("/Users/someone").0, "other");
        assert_eq!(SafeToken::of(&"x".repeat(41)).0, "other");
    }

    #[test]
    fn a_hash_depends_on_the_key() {
        let first = Hashed::with_key((1, 2), "src/Payroll.kt");
        assert_eq!(first, Hashed::with_key((1, 2), "src/Payroll.kt"));
        assert_ne!(first, Hashed::with_key((3, 4), "src/Payroll.kt"));
    }

    #[test]
    fn only_this_programs_frames_are_kept_with_their_source_locations() {
        let backtrace = "   0: std::backtrace::Backtrace::create\n             at /rustc/abc/library/std/src/backtrace.rs:331:13\n   1: infer_body\n             at /Users/builder/kotlin-analyzer/crates/hir-ty/src/infer/units.rs:170:5\n   2: fetch<salsa::Foo>\n             at /Users/builder/.cargo/registry/src/index.crates.io-1/salsa-0.28.5/src/function.rs:10:1\n   3: diagnostics\n             at ./crates/ide/src/diagnostics/mod.rs:304:17\n";
        let frames = own_frames(backtrace, 10);
        let rendered: Vec<String> = frames
            .iter()
            .map(|frame| format!("{} {}", frame.function, frame.location))
            .collect();
        assert_eq!(
            rendered,
            [
                "infer_body crates/hir-ty/src/infer/units.rs:170:5",
                "diagnostics crates/ide/src/diagnostics/mod.rs:304:17",
            ]
        );
    }
}
