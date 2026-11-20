//! `textDocument/codeLens` and `codeLens/resolve`. A test lens comes
//! whole; references, what a member overrides (↑) and implementations (↓)
//! are found only when the editor resolves their lens (the visible ones),
//! from where the declaration's name is and which they are (`data`).
//!
//! The lenses' commands are the client's to run — the editor extensions
//! implement them:
//!
//! - `kotlin-analyzer.runTests` with `{ "uri", "filter" }`: Gradle's test
//!   task in the project holding the file, with `--tests <filter>`;
//! - `kotlin-analyzer.showReferences` with the declaration's URI, the
//!   position of its name and the locations to show — or, as
//!   `codeLens.locationsCommand` says, `editor.action.showReferences` with
//!   the same, which Zed runs itself.
//!
//! A library file rendered from class files gets one more, over its first
//! line, with no command: a banner saying where it comes from and why.

use ide::{Cancellable, CodeLensKind, TestTarget};
use serde_json::{Value, json};
use vfs::LineIndex;

use super::convert;
use super::handlers::Snapshot;

pub(super) const CODE_LENS: &str = "textDocument/codeLens";
pub(super) const RESOLVE: &str = "codeLens/resolve";
pub(super) const METHODS: [&str; 2] = [CODE_LENS, RESOLVE];

pub(crate) const RUN_TESTS_COMMAND: &str = "kotlin-analyzer.runTests";
pub(crate) const SHOW_REFERENCES_COMMAND: &str = "kotlin-analyzer.showReferences";
/// VS Code's command showing locations, which Zed runs itself.
const EDITOR_SHOW_REFERENCES_COMMAND: &str = "editor.action.showReferences";

/// Which lenses the client wants: `codeLens.references`,
/// `codeLens.hierarchy`, `codeLens.tests` in the settings; and the command
/// showing locations, `codeLens.locationsCommand`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CodeLensConfig {
    pub(crate) references: bool,
    /// What a member overrides (↑), and what implements a class or member (↓).
    pub(crate) hierarchy: bool,
    pub(crate) tests: bool,
    pub(crate) locations_command: LocationsCommand,
}

impl Default for CodeLensConfig {
    fn default() -> CodeLensConfig {
        CodeLensConfig {
            references: true,
            hierarchy: true,
            tests: true,
            locations_command: LocationsCommand::KotlinAnalyzer,
        }
    }
}

/// The command of a lens showing locations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LocationsCommand {
    /// `kotlin-analyzer.showReferences`, which the editor extensions run.
    KotlinAnalyzer,
    /// `editor.action.showReferences`, which Zed runs itself.
    Editor,
}

impl LocationsCommand {
    pub(crate) fn from_setting(name: &str) -> Option<LocationsCommand> {
        match name {
            SHOW_REFERENCES_COMMAND => Some(LocationsCommand::KotlinAnalyzer),
            EDITOR_SHOW_REFERENCES_COMMAND => Some(LocationsCommand::Editor),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            LocationsCommand::KotlinAnalyzer => SHOW_REFERENCES_COMMAND,
            LocationsCommand::Editor => EDITOR_SHOW_REFERENCES_COMMAND,
        }
    }
}

/// What a lens resolved later finds, as its `data` names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Found {
    References,
    Overridden,
    Implementations,
}

impl Found {
    fn name(self) -> &'static str {
        match self {
            Found::References => "references",
            Found::Overridden => "overridden",
            Found::Implementations => "implementations",
        }
    }

    fn from_name(name: &str) -> Option<Found> {
        [Found::References, Found::Overridden, Found::Implementations]
            .into_iter()
            .find(|found| found.name() == name)
    }
}

pub(super) fn handle(snapshot: &Snapshot, method: &str, params: &Value) -> Cancellable<Value> {
    if method == CODE_LENS
        && let Some(banner) = decompiled_banner(params)
    {
        return Ok(json!([banner]));
    }
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(Value::Null);
    };
    let Some(uri) = snapshot
        .paths
        .get(file.index())
        .and_then(|path| convert::uri_of(path))
    else {
        return Ok(Value::Null);
    };
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    if method == RESOLVE {
        return resolve(snapshot, file, uri.as_str(), &text, &index, params.clone());
    }
    let config = snapshot.code_lens_config;
    let mut lenses = Vec::new();
    for lens in snapshot.analysis.code_lenses(file)? {
        let range = convert::range(&index, &text, lens.range);
        match lens.kind {
            CodeLensKind::RunTests { target, filter } if config.tests => {
                let title = match target {
                    TestTarget::Function => "▶ Run test",
                    TestTarget::Class => "▶ Run tests",
                };
                lenses.push(json!({
                    "range": range,
                    "command": {
                        "title": title,
                        "command": RUN_TESTS_COMMAND,
                        "arguments": [{ "uri": uri.as_str(), "filter": filter }],
                    },
                }));
            }
            CodeLensKind::References if config.references => {
                lenses.push(unresolved(
                    &range,
                    uri.as_str(),
                    lens.range.start,
                    Found::References,
                ));
            }
            CodeLensKind::Overridden if config.hierarchy => {
                lenses.push(unresolved(
                    &range,
                    uri.as_str(),
                    lens.range.start,
                    Found::Overridden,
                ));
            }
            CodeLensKind::Implementations if config.hierarchy => {
                lenses.push(unresolved(
                    &range,
                    uri.as_str(),
                    lens.range.start,
                    Found::Implementations,
                ));
            }
            CodeLensKind::RunTests { .. }
            | CodeLensKind::References
            | CodeLensKind::Overridden
            | CodeLensKind::Implementations => {}
        }
    }
    Ok(Value::Array(lenses))
}

/// A lens to resolve later: what it finds, from where the name is.
fn unresolved(range: &lsp_types::Range, uri: &str, offset: u32, found: Found) -> Value {
    json!({
        "range": range,
        "data": { "textDocument": { "uri": uri }, "offset": offset, "found": found.name() },
    })
}

/// The banner lens of a file rendered from class files: its notice, over
/// the first line, with a command that does nothing.
fn decompiled_banner(params: &Value) -> Option<Value> {
    let uri: lsp_types::Uri = params["textDocument"]["uri"].as_str()?.parse().ok()?;
    let path = convert::path_of(&uri)?;
    let notice = super::library_sources::decompiled_notice(&path)?;
    Some(json!({
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        "command": { "title": notice, "command": "" },
    }))
}

/// A lens with what it found: its title, and the command showing where.
fn resolve(
    snapshot: &Snapshot,
    file: vfs::FileId,
    uri: &str,
    text: &str,
    index: &LineIndex,
    mut lens: Value,
) -> Cancellable<Value> {
    let Some(offset) = lens["data"]["offset"].as_u64() else {
        return Ok(lens);
    };
    let offset = offset as usize;
    // Lenses from before `found` was written count references.
    let found = lens["data"]["found"]
        .as_str()
        .map_or(Some(Found::References), Found::from_name);
    let Some(found) = found else {
        return Ok(lens);
    };
    let (title, locations) = match found {
        Found::References => {
            let references = snapshot.analysis.references(file, offset, false)?;
            let locations = reference_locations(snapshot, &references)?;
            let title = match locations.len() {
                1 => "1 reference".to_owned(),
                count => format!("{count} references"),
            };
            (title, locations)
        }
        Found::Overridden => {
            let targets = snapshot.analysis.goto_super(file, offset)?;
            let locations = target_locations(snapshot, &targets)?;
            (overridden_title(&targets), locations)
        }
        Found::Implementations => {
            let targets = snapshot.analysis.goto_implementation(file, offset)?;
            let locations = target_locations(snapshot, &targets)?;
            let title = match locations.len() {
                0 => "no implementations".to_owned(),
                1 => "↓ 1 implementation".to_owned(),
                count => format!("↓ {count} implementations"),
            };
            (title, locations)
        }
    };
    lens["command"] = if locations.is_empty() {
        json!({ "title": title, "command": "" })
    } else {
        json!({
            "title": title,
            "command": snapshot.code_lens_config.locations_command.name(),
            "arguments": [uri, convert::position(index, text, offset), locations],
        })
    };
    Ok(lens)
}

/// `↑ Base.name`, `+N` for more: what a member overrides directly.
fn overridden_title(targets: &[ide::NavigationTarget]) -> String {
    let Some(first) = targets.first() else {
        return "↑ overrides nothing found".to_owned();
    };
    let shown = match &first.container {
        Some(container) => format!("↑ {container}.{}", first.name),
        None => format!("↑ {}", first.name),
    };
    match targets.len() {
        1 => shown,
        count => format!("{shown} +{}", count - 1),
    }
}

fn reference_locations(
    snapshot: &Snapshot,
    references: &[ide::FileRange],
) -> Cancellable<Vec<Value>> {
    let mut locations = Vec::with_capacity(references.len());
    for reference in references {
        let Some(reference_text) = snapshot.analysis.file_text(reference.file)? else {
            continue;
        };
        let reference_uri = snapshot
            .paths
            .get(reference.file.index())
            .and_then(|path| convert::uri_of(path));
        let Some(reference_uri) = reference_uri else {
            continue;
        };
        let reference_index = LineIndex::with_unit(&reference_text, snapshot.encoding);
        locations.push(json!({
            "uri": reference_uri.as_str(),
            "range": convert::range(&reference_index, &reference_text, reference.range),
        }));
    }
    Ok(locations)
}

fn target_locations(
    snapshot: &Snapshot,
    targets: &[ide::NavigationTarget],
) -> Cancellable<Vec<Value>> {
    let mut locations = Vec::with_capacity(targets.len());
    for target in targets {
        if let Some(ranges) = super::handlers::target_ranges(snapshot, target)? {
            locations.push(json!({ "uri": ranges.uri.as_str(), "range": ranges.focus }));
        }
    }
    Ok(locations)
}
