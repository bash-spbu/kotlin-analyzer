//! Pull diagnostics: `textDocument/diagnostic` and `workspace/diagnostic`,
//! for a client that asks for them rather than being sent them. A file's
//! report id stands for what its diagnostics are made from: its own text,
//! every workspace file's interface (`ide::FileInterface`), and the model
//! and settings (`Snapshot::revision`). Asked again with the id it has, a
//! file is "unchanged" and isn't checked: after an edit inside a body, only
//! the edited file is.

use std::hash::{Hash, Hasher};

use ide::Cancellable;
use lsp_types::{
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, FullDocumentDiagnosticReport,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    UnchangedDocumentDiagnosticReport, WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport, WorkspaceFullDocumentDiagnosticReport,
    WorkspaceUnchangedDocumentDiagnosticReport,
};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHasher};

use super::progress::Progress;
use serde_json::Value;
use vfs::FileId;

use super::convert;
use super::handlers::{self, Snapshot};

/// What the reports were made from: each file's text (hashed) and
/// interface, and how many times an interface changed — a file added or
/// removed, or its declarations or inferred types changed.
#[derive(Default)]
pub(crate) struct ReportBasis {
    files: FxHashMap<FileId, FileBasis>,
    interface_changes: u64,
}

struct FileBasis {
    text: u64,
    interface: Option<ide::FileInterface>,
}

/// Which files a report id is asked of: some (a document's), or all the
/// workspace's — when a file that was there is gone, an interface changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Some,
    All,
}

/// Each of `files`' report ids, after taking in what changed since the
/// basis was last brought up to date.
fn report_ids(
    snapshot: &Snapshot,
    files: &[FileId],
    asked: Asked,
) -> Cancellable<FxHashMap<FileId, String>> {
    let mut texts = Vec::with_capacity(files.len());
    for file in files {
        let text = snapshot.analysis.file_text(*file)?.unwrap_or_default();
        let mut hasher = FxHasher::default();
        text.hash(&mut hasher);
        texts.push((*file, hasher.finish()));
    }
    // Interfaces are worked out unlocked: a first report of every file
    // reads all of them, and a document's report shouldn't wait for it.
    let unknown: Vec<FileId> = {
        let basis = snapshot
            .report_basis
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        texts
            .iter()
            .filter(|(file, text)| {
                basis
                    .files
                    .get(file)
                    .is_none_or(|known| known.text != *text)
            })
            .map(|(file, _)| *file)
            .collect()
    };
    let mut worked_out = FxHashMap::default();
    for file in unknown {
        worked_out.insert(file, snapshot.analysis.file_interface(file)?);
    }
    let mut basis = snapshot
        .report_basis
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut interface_changed = false;
    for (file, text) in &texts {
        let Some(interface) = worked_out.remove(file) else {
            continue;
        };
        let changed = basis
            .files
            .get(file)
            .is_none_or(|known| known.interface != interface);
        interface_changed |= changed;
        basis.files.insert(
            *file,
            FileBasis {
                text: *text,
                interface,
            },
        );
    }
    if asked == Asked::All {
        let before = basis.files.len();
        basis.files.retain(|file, _| files.contains(file));
        interface_changed |= basis.files.len() != before;
    }
    if interface_changed {
        basis.interface_changes += 1;
    }
    let interface_changes = basis.interface_changes;
    Ok(texts
        .into_iter()
        .map(|(file, text)| {
            let id = format!("{}.{interface_changes}.{text:016x}", snapshot.revision);
            (file, id)
        })
        .collect())
}

/// `textDocument/diagnostic`.
pub(super) fn document(
    snapshot: &Snapshot,
    params: lsp_types::DocumentDiagnosticParams,
) -> Cancellable<Value> {
    let Some(file) = snapshot.request_file else {
        return Ok(Value::Null);
    };
    let ids = report_ids(snapshot, &[file], Asked::Some)?;
    let id = ids.get(&file).cloned().unwrap_or_default();
    let report = if params.previous_result_id.as_deref() == Some(id.as_str()) {
        DocumentDiagnosticReport::Unchanged(RelatedUnchangedDocumentDiagnosticReport {
            related_documents: None,
            unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                result_id: id,
            },
        })
    } else {
        DocumentDiagnosticReport::Full(RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: Some(id),
                items: diagnostics_of(snapshot, file)?,
            },
        })
    };
    let result = DocumentDiagnosticReportResult::Report(report);
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// A workspace check to make: the files whose report the client has
/// ("unchanged"), and those to check again, each with its URI and id.
pub(super) struct WorkspacePlan {
    pub(super) unchanged: Vec<WorkspaceDocumentDiagnosticReport>,
    pub(super) to_check: Vec<FileToCheck>,
}

pub(super) struct FileToCheck {
    file: FileId,
    uri: lsp_types::Uri,
    id: String,
}

/// `workspace/diagnostic`'s first step: which files the client's reports
/// leave current, which to check again. The checking goes in batches, the
/// main loop dropping what the last one left beyond each cache's limit
/// before the next (`GlobalState::next_check_batch`): one pass over a large
/// workspace doesn't keep every file's analysis.
pub(super) fn plan(
    snapshot: &Snapshot,
    params: lsp_types::WorkspaceDiagnosticParams,
) -> Cancellable<WorkspacePlan> {
    if !snapshot.loaded {
        return Ok(WorkspacePlan {
            unchanged: Vec::new(),
            to_check: Vec::new(),
        });
    }
    let workspace_files = snapshot.analysis.workspace_files()?;
    let ids = report_ids(snapshot, &workspace_files, Asked::All)?;
    // By the URI's text: `Uri` caches its parts inside, no key for a map.
    let previous: FxHashMap<String, String> = params
        .previous_result_ids
        .into_iter()
        .map(|previous| (previous.uri.as_str().to_owned(), previous.value))
        .collect();
    let mut plan = WorkspacePlan {
        unchanged: Vec::new(),
        to_check: Vec::new(),
    };
    for file in workspace_files {
        let Some(uri) = snapshot
            .paths
            .get(file.index())
            .and_then(|path| convert::uri_of(path))
        else {
            continue;
        };
        let Some(id) = ids.get(&file).cloned() else {
            continue;
        };
        let current = previous.get(uri.as_str()) == Some(&id);
        if current {
            plan.unchanged
                .push(WorkspaceDocumentDiagnosticReport::Unchanged(
                    WorkspaceUnchangedDocumentDiagnosticReport {
                        uri,
                        version: None,
                        unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                            result_id: id,
                        },
                    },
                ));
        } else {
            plan.to_check.push(FileToCheck { file, uri, id });
        }
    }
    Ok(plan)
}

/// Checks `batch` in parallel, counting each file checked in `checked`
/// towards the progress's `total`.
pub(super) fn check_batch(
    snapshot: &Snapshot,
    batch: Vec<FileToCheck>,
    progress: &Progress,
    checked: &AtomicUsize,
    total: usize,
) -> Cancellable<Vec<WorkspaceDocumentDiagnosticReport>> {
    let threads = rayon::current_num_threads().max(1);
    let chunk = batch.len().div_ceil(threads * 4).max(1);
    let mut batch = batch;
    let mut parts: Vec<(Snapshot, Vec<FileToCheck>)> = Vec::new();
    while !batch.is_empty() {
        let rest = batch.split_off(chunk.min(batch.len()));
        parts.push((snapshot.with_analysis_clone(), batch));
        batch = rest;
    }
    let reports: Vec<Cancellable<Vec<WorkspaceDocumentDiagnosticReport>>> = parts
        .into_par_iter()
        .map(|(part, files)| {
            let mut reports = Vec::with_capacity(files.len());
            for FileToCheck { file, uri, id } in files {
                reports.push(WorkspaceDocumentDiagnosticReport::Full(
                    WorkspaceFullDocumentDiagnosticReport {
                        uri,
                        version: None,
                        full_document_diagnostic_report: FullDocumentDiagnosticReport {
                            result_id: Some(id),
                            items: diagnostics_of(&part, file)?,
                        },
                    },
                ));
                let done = checked.fetch_add(1, Ordering::Relaxed) + 1;
                progress.report(
                    format!("{done}/{total}"),
                    super::progress::percentage(done, total),
                );
            }
            Ok(reports)
        })
        .collect();
    let mut items = Vec::new();
    for part in reports {
        items.extend(part?);
    }
    Ok(items)
}

/// The answer to `workspace/diagnostic`: every file's report.
pub(super) fn workspace_answer(items: Vec<WorkspaceDocumentDiagnosticReport>) -> Value {
    let result = WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items });
    serde_json::to_value(result).unwrap_or_default()
}

/// A file's diagnostics: syntax alone until the workspace has loaded.
fn diagnostics_of(snapshot: &Snapshot, file: FileId) -> Cancellable<Vec<lsp_types::Diagnostic>> {
    let syntax_only = ide::DiagnosticsConfig {
        only: vec![ide::Check::Syntax],
        ..ide::DiagnosticsConfig::default()
    };
    let config = if snapshot.loaded {
        &*snapshot.diagnostics_config
    } else {
        &syntax_only
    };
    handlers::diagnostics(&snapshot.analysis, file, config, snapshot.encoding)
}
