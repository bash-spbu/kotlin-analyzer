//! The host side of agents' queries (`crate::agent`): the server listens
//! on a socket per workspace folder, and answers each query from a
//! snapshot, as it answers an editor's requests — the open documents'
//! unsaved text included. Answers are plain text for a language model:
//! `path:line:column`, relative to the root inside it, with the line.

use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;
use ide::{Cancellable, NavigationFile, NavigationTarget};
use vfs::FileId;

use super::Task;
use super::handlers::Snapshot;
use super::library_sources;
use crate::agent::protocol::{self, AgentQuery, AgentReply, AgentRequest, SourcePosition};

/// The most references an answer lists.
const REFERENCE_LIMIT: usize = 200;
/// The most symbols a search answers with.
const SYMBOL_LIMIT: usize = 50;

/// A socket the server listens on; its file goes when the server does.
pub(super) struct ListeningSocket {
    path: PathBuf,
}

impl Drop for ListeningSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where an agent's answer goes: the connection it asked on.
#[derive(Clone)]
pub(super) struct Replier {
    stream: Arc<Mutex<UnixStream>>,
}

impl Replier {
    pub(super) fn send(&self, reply: &AgentReply) {
        if let Ok(mut stream) = self.stream.lock() {
            // A connection gone is an agent gone: nothing to tell.
            let _ = protocol::write_line(&mut *stream, reply);
        }
    }
}

/// Listens for agents on `directory`'s socket of each root another live
/// host doesn't listen on already; each request becomes a task.
pub(super) fn listen(
    directory: &Path,
    roots: &[PathBuf],
    tasks: &Sender<Task>,
) -> Vec<ListeningSocket> {
    if std::fs::create_dir_all(directory).is_err() {
        return Vec::new();
    }
    let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
    let mut sockets = Vec::new();
    for root in roots {
        let path = protocol::socket_path(directory, root);
        if path.as_os_str().len() > protocol::SOCKET_PATH_LIMIT {
            crate::log::Event::warn("agent-socket-path-too-long")
                .count("length", path.as_os_str().len())
                .emit();
            continue;
        }
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                // Another host answers for this root.
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
        let Ok(listener) = UnixListener::bind(&path) else {
            continue;
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let tasks = tasks.clone();
        std::thread::Builder::new()
            .name("kotlin-analyzer-agent-socket".to_owned())
            .spawn(move || accept(&listener, &tasks))
            .ok();
        sockets.push(ListeningSocket { path });
    }
    sockets
}

fn accept(listener: &UnixListener, tasks: &Sender<Task>) {
    for stream in listener.incoming().flatten() {
        let tasks = tasks.clone();
        std::thread::Builder::new()
            .name("kotlin-analyzer-agent".to_owned())
            .spawn(move || serve(stream, &tasks))
            .ok();
    }
}

/// Reads one connection's requests until it closes.
fn serve(stream: UnixStream, tasks: &Sender<Task>) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let replier = Replier {
        stream: Arc::new(Mutex::new(writer)),
    };
    let mut reader = BufReader::new(stream);
    loop {
        match protocol::read_line::<AgentRequest>(&mut reader) {
            Ok(Some(request)) => {
                let task = Task::Agent {
                    request,
                    replier: replier.clone(),
                };
                if tasks.send(task).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                replier.send(&AgentReply::of(
                    0,
                    Err(format!("unreadable request: {error}")),
                ));
            }
            Err(_) => return,
        }
    }
}

/// The answer to `query`, its file (if any) being `snapshot.request_file`;
/// paths shown relative to `root` when inside it.
pub(super) fn answer(
    snapshot: &Snapshot,
    root: &Path,
    query: &AgentQuery,
) -> Cancellable<Result<String, String>> {
    let answers = Answers { snapshot, root };
    if let AgentQuery::Symbols { query } = query {
        return answers.symbols(query);
    }
    let Some(file) = snapshot.request_file else {
        let named = query
            .file()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        return Ok(Err(format!(
            "{named} isn't a Kotlin or Java file of the project at {}",
            root.display()
        )));
    };
    match query {
        AgentQuery::Symbols { .. } => unreachable!("answered above"),
        AgentQuery::Definition { at } => answers.at(file, at, |offset| {
            let targets = snapshot.analysis.goto_definition(file, offset)?;
            answers.targets(&targets, "No declaration found")
        }),
        AgentQuery::Implementations { at } => answers.at(file, at, |offset| {
            let targets = snapshot.analysis.goto_implementation(file, offset)?;
            answers.targets(&targets, "No implementations found")
        }),
        AgentQuery::References { at } => {
            answers.at(file, at, |offset| answers.references(file, offset))
        }
        AgentQuery::Hover { at } => answers.at(file, at, |offset| {
            Ok(match snapshot.analysis.hover(file, offset)? {
                Some(hover) => Ok(hover.markdown),
                None => Err("Nothing known about the name there".to_owned()),
            })
        }),
        AgentQuery::Outline { .. } => answers.outline(file),
        AgentQuery::Diagnostics { .. } => answers.diagnostics(file),
    }
}

struct Answers<'snapshot> {
    snapshot: &'snapshot Snapshot,
    root: &'snapshot Path,
}

impl Answers<'_> {
    /// `answer` at the offset `at` names, or why there is none.
    fn at(
        &self,
        file: FileId,
        at: &SourcePosition,
        answer: impl FnOnce(usize) -> Cancellable<Result<String, String>>,
    ) -> Cancellable<Result<String, String>> {
        let Some(text) = self.snapshot.analysis.file_text(file)? else {
            return Ok(Err("the file isn't loaded".to_owned()));
        };
        match offset_of(&text, at) {
            Ok(offset) => answer(offset),
            Err(why) => Ok(Err(why)),
        }
    }

    fn symbols(&self, query: &str) -> Cancellable<Result<String, String>> {
        let targets = self
            .snapshot
            .analysis
            .workspace_symbols(query, SYMBOL_LIMIT)?;
        self.targets(&targets, &format!("No declaration matches `{query}`"))
    }

    /// Each target as `path:line:column — kind name in container`, with
    /// its line.
    fn targets(
        &self,
        targets: &[NavigationTarget],
        none: &str,
    ) -> Cancellable<Result<String, String>> {
        if targets.is_empty() {
            return Ok(Err(none.to_owned()));
        }
        let mut lines = Vec::new();
        for target in targets {
            let Some((path, text)) = self.target_source(target)? else {
                let container = target.container.as_deref().unwrap_or("");
                lines.push(format!(
                    "(compiled, no sources) {} {container}.{}",
                    kind_name(target.kind),
                    target.name
                ));
                continue;
            };
            let place = Place::of(&text, target.focus_range.start as usize);
            let container = target
                .container
                .as_deref()
                .map(|container| format!(" in {container}"))
                .unwrap_or_default();
            let decompiled = match &target.file {
                NavigationFile::Library {
                    origin: hir::SourceOrigin::Decompiled(_),
                    ..
                } => " (no sources: declarations rendered from the class files)",
                _ => "",
            };
            lines.push(format!(
                "{}:{}:{} — {} {}{container}{decompiled}\n    {}",
                self.shown(&path),
                place.line,
                place.column,
                kind_name(target.kind),
                target.name,
                place.text.trim()
            ));
        }
        Ok(Ok(lines.join("\n")))
    }

    fn target_source(
        &self,
        target: &NavigationTarget,
    ) -> Cancellable<Option<(PathBuf, std::sync::Arc<str>)>> {
        Ok(match &target.file {
            NavigationFile::Workspace(file) => {
                let text = self.snapshot.analysis.file_text(*file)?;
                let path = self.snapshot.paths.get(file.index()).cloned();
                path.zip(text)
            }
            NavigationFile::Library {
                archive,
                entry,
                text,
                ..
            } => {
                library_sources::materialize(archive, entry, text).map(|path| (path, text.clone()))
            }
        })
    }

    fn references(&self, file: FileId, offset: usize) -> Cancellable<Result<String, String>> {
        let references = self.snapshot.analysis.references(file, offset, false)?;
        if references.is_empty() {
            return Ok(Err("No references found".to_owned()));
        }
        let mut lines = vec![format!("{} references", references.len())];
        for reference in references.iter().take(REFERENCE_LIMIT) {
            let Some(text) = self.snapshot.analysis.file_text(reference.file)? else {
                continue;
            };
            let Some(path) = self.snapshot.paths.get(reference.file.index()) else {
                continue;
            };
            let place = Place::of(&text, reference.range.start as usize);
            lines.push(format!(
                "{}:{}:{}: {}",
                self.shown(path),
                place.line,
                place.column,
                place.text.trim()
            ));
        }
        if references.len() > REFERENCE_LIMIT {
            lines.push(format!("… and {} more", references.len() - REFERENCE_LIMIT));
        }
        Ok(Ok(lines.join("\n")))
    }

    fn outline(&self, file: FileId) -> Cancellable<Result<String, String>> {
        let Some(text) = self.snapshot.analysis.file_text(file)? else {
            return Ok(Err("the file isn't loaded".to_owned()));
        };
        let symbols = self.snapshot.analysis.document_symbols(file)?;
        if symbols.is_empty() {
            return Ok(Err("The file declares nothing".to_owned()));
        }
        fn write(
            text: &str,
            symbols: &[ide::DocumentSymbol],
            depth: usize,
            lines: &mut Vec<String>,
        ) {
            for symbol in symbols {
                let line = Place::of(text, symbol.selection_range.start as usize).line;
                let detail = symbol.detail.as_deref().unwrap_or("");
                lines.push(format!(
                    "{}{} {}{detail} (line {line})",
                    "  ".repeat(depth),
                    kind_name(symbol.kind),
                    symbol.name
                ));
                write(text, &symbol.children, depth + 1, lines);
            }
        }
        let mut lines = Vec::new();
        write(&text, &symbols, 0, &mut lines);
        Ok(Ok(lines.join("\n")))
    }

    fn diagnostics(&self, file: FileId) -> Cancellable<Result<String, String>> {
        let Some(text) = self.snapshot.analysis.file_text(file)? else {
            return Ok(Err("the file isn't loaded".to_owned()));
        };
        let diagnostics = self
            .snapshot
            .analysis
            .diagnostics(file, &self.snapshot.diagnostics_config)?;
        if diagnostics.is_empty() {
            return Ok(Ok("No problems found".to_owned()));
        }
        let lines: Vec<String> = diagnostics
            .iter()
            .map(|diagnostic| {
                let place = Place::of(&text, diagnostic.range.start as usize);
                let severity = match diagnostic.severity {
                    ide::Severity::Error => "error",
                    ide::Severity::Warning | ide::Severity::Lint => "warning",
                    ide::Severity::Unnecessary => "hint",
                };
                format!(
                    "{}:{}: {severity} [{}] {}",
                    place.line,
                    place.column,
                    diagnostic.check.name(),
                    diagnostic.message
                )
            })
            .collect();
        Ok(Ok(lines.join("\n")))
    }

    /// `path` relative to the root when inside it.
    fn shown(&self, path: &Path) -> String {
        path.strip_prefix(self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// A line (from 1), a column (from 1, in characters) and the line's text.
struct Place<'text> {
    line: usize,
    column: usize,
    text: &'text str,
}

impl<'text> Place<'text> {
    fn of(text: &'text str, offset: usize) -> Place<'text> {
        let offset = offset.min(text.len());
        let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
        let line_end = text[offset..]
            .find('\n')
            .map_or(text.len(), |index| offset + index);
        Place {
            line: text[..offset].matches('\n').count() + 1,
            column: text[line_start..offset].chars().count() + 1,
            text: &text[line_start..line_end],
        }
    }
}

/// The offset `at` names: its name on its line (as a whole word), or its
/// column.
fn offset_of(text: &str, at: &SourcePosition) -> Result<usize, String> {
    let line_number = at.line.max(1) as usize;
    let mut line_start = 0;
    for _ in 1..line_number {
        match text[line_start..].find('\n') {
            Some(index) => line_start += index + 1,
            None => return Err(format!("the file has fewer than {line_number} lines")),
        }
    }
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |index| line_start + index);
    let line = &text[line_start..line_end];
    if let Some(symbol) = at.symbol.as_deref().filter(|symbol| !symbol.is_empty()) {
        let symbol = symbol.trim_matches('`');
        return word_in(line, symbol)
            .map(|index| line_start + index)
            .ok_or_else(|| format!("`{symbol}` isn't on line {line_number}: {}", line.trim()));
    }
    let Some(column) = at.column else {
        return Err("name the symbol on the line, or give its column".to_owned());
    };
    let column = column.max(1) as usize;
    line.char_indices()
        .nth(column - 1)
        .map(|(index, _)| line_start + index)
        .ok_or_else(|| format!("line {line_number} has fewer than {column} characters"))
}

/// Where `word` stands on `line` as a whole word (not inside a longer
/// name).
fn word_in(line: &str, word: &str) -> Option<usize> {
    let is_word_char = |char: char| char.is_alphanumeric() || char == '_';
    line.match_indices(word)
        .map(|(index, _)| index)
        .find(|index| {
            let before = line[..*index].chars().next_back();
            let after = line[index + word.len()..].chars().next();
            !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
        })
}

fn kind_name(kind: ide::SymbolKind) -> &'static str {
    match kind {
        ide::SymbolKind::Class => "class",
        ide::SymbolKind::Interface => "interface",
        ide::SymbolKind::Enum => "enum",
        ide::SymbolKind::Object => "object",
        ide::SymbolKind::Annotation => "annotation",
        ide::SymbolKind::Function => "function",
        ide::SymbolKind::Method => "method",
        ide::SymbolKind::Property => "property",
        ide::SymbolKind::Field => "field",
        ide::SymbolKind::Constructor => "constructor",
        ide::SymbolKind::EnumMember => "enum entry",
        ide::SymbolKind::TypeAlias => "typealias",
        ide::SymbolKind::Variable => "variable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: u32, symbol: Option<&str>, column: Option<u32>) -> SourcePosition {
        SourcePosition {
            file: PathBuf::from("Order.kt"),
            line,
            symbol: symbol.map(str::to_owned),
            column,
        }
    }

    #[test]
    fn a_position_is_a_whole_word_on_its_line_or_a_column() {
        let text = "class Order {\n    val totals = total(1) + subtotal\n}\n";
        let line_two = text.find("    val").unwrap();
        assert_eq!(
            offset_of(text, &at(2, Some("total"), None)),
            Ok(text.find("total(").unwrap())
        );
        assert_eq!(
            offset_of(text, &at(2, Some("`subtotal`"), None)),
            Ok(text.find("subtotal").unwrap())
        );
        assert_eq!(offset_of(text, &at(2, None, Some(9))), Ok(line_two + 8));
        assert!(
            offset_of(text, &at(2, Some("price"), None))
                .unwrap_err()
                .contains("isn't on line 2")
        );
        assert!(offset_of(text, &at(9, Some("x"), None)).is_err());
        assert!(offset_of(text, &at(2, None, None)).is_err());
    }

    #[test]
    fn a_place_counts_lines_and_characters_from_one() {
        let text = "a\nval é = total\n";
        let place = Place::of(text, text.find("total").unwrap());
        assert_eq!(
            (place.line, place.column, place.text),
            (2, 9, "val é = total")
        );
    }
}
