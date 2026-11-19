//! `textDocument/codeLens` and `codeLens/resolve`. A test lens comes
//! whole; a reference count is counted only when the editor resolves its
//! lens (the visible ones), from where the declaration's name is (`data`).
//!
//! Both lenses' commands are the client's to run — the editor extensions
//! implement them:
//!
//! - `kotlin-analyzer.runTests` with `{ "uri", "filter" }`: Gradle's test
//!   task in the project holding the file, with `--tests <filter>`;
//! - `kotlin-analyzer.showReferences` with the declaration's URI, the
//!   position of its name and the references' locations.
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

/// Which lenses the client wants: `codeLens.references`, `codeLens.tests`
/// in the settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CodeLensConfig {
    pub(crate) references: bool,
    pub(crate) tests: bool,
}

impl Default for CodeLensConfig {
    fn default() -> CodeLensConfig {
        CodeLensConfig {
            references: true,
            tests: true,
        }
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
                lenses.push(json!({
                    "range": range,
                    "data": { "textDocument": { "uri": uri.as_str() }, "offset": lens.range.start },
                }));
            }
            CodeLensKind::RunTests { .. } | CodeLensKind::References => {}
        }
    }
    Ok(Value::Array(lenses))
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

/// A reference lens with its count and the command showing them.
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
    let references = snapshot.analysis.references(file, offset, false)?;
    let mut locations = Vec::with_capacity(references.len());
    for reference in &references {
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
    let title = match locations.len() {
        1 => "1 reference".to_owned(),
        count => format!("{count} references"),
    };
    lens["command"] = json!({
        "title": title,
        "command": SHOW_REFERENCES_COMMAND,
        "arguments": [uri, convert::position(index, text, offset), locations],
    });
    Ok(lens)
}
