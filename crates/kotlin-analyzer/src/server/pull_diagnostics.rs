//! Pull diagnostics: `textDocument/diagnostic` and `workspace/diagnostic`,
//! for a client that asks for them rather than being sent them. A report's
//! id is the workspace's revision: asked again before anything changed,
//! the answer is "unchanged".

use std::sync::Arc;

use ide::Cancellable;
use lsp_types::{
    DocumentDiagnosticReport, DocumentDiagnosticReportResult, FullDocumentDiagnosticReport,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    UnchangedDocumentDiagnosticReport, WorkspaceDiagnosticReport, WorkspaceDiagnosticReportResult,
    WorkspaceDocumentDiagnosticReport, WorkspaceFullDocumentDiagnosticReport,
    WorkspaceUnchangedDocumentDiagnosticReport,
};
use rayon::prelude::*;
use serde_json::Value;
use vfs::FileId;

use super::convert;
use super::handlers::{self, Snapshot};

/// `textDocument/diagnostic`.
pub(super) fn document(
    snapshot: &Snapshot,
    params: lsp_types::DocumentDiagnosticParams,
) -> Cancellable<Value> {
    let Some(file) = snapshot.request_file else {
        return Ok(Value::Null);
    };
    let id = snapshot.revision.to_string();
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

/// `workspace/diagnostic`: every workspace file's, checked in parallel —
/// "unchanged" for those the client has from this revision.
pub(super) fn workspace(
    snapshot: &Snapshot,
    params: lsp_types::WorkspaceDiagnosticParams,
) -> Cancellable<Value> {
    if !snapshot.loaded {
        let empty = WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport {
            items: Vec::new(),
        });
        return Ok(serde_json::to_value(empty).unwrap_or_default());
    }
    let id = snapshot.revision.to_string();
    let current: Vec<lsp_types::Uri> = params
        .previous_result_ids
        .into_iter()
        .filter(|previous| previous.value == id)
        .map(|previous| previous.uri)
        .collect();
    let files: Vec<(FileId, lsp_types::Uri)> = snapshot
        .analysis
        .workspace_files()?
        .into_iter()
        .filter_map(|file| {
            let uri = convert::uri_of(snapshot.paths.get(file.index())?)?;
            Some((file, uri))
        })
        .collect();
    let threads = rayon::current_num_threads().max(1);
    let chunk = files.len().div_ceil(threads * 4).max(1);
    let shared = Arc::new(current);
    let reports: Vec<Cancellable<Vec<WorkspaceDocumentDiagnosticReport>>> = files
        .chunks(chunk)
        .map(|chunk| {
            (
                snapshot.with_analysis_clone(),
                chunk.to_vec(),
                shared.clone(),
            )
        })
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(part, chunk, current)| {
            let mut reports = Vec::with_capacity(chunk.len());
            for (file, uri) in chunk {
                let report = if current.contains(&uri) {
                    WorkspaceDocumentDiagnosticReport::Unchanged(
                        WorkspaceUnchangedDocumentDiagnosticReport {
                            uri,
                            version: None,
                            unchanged_document_diagnostic_report:
                                UnchangedDocumentDiagnosticReport {
                                    result_id: part.revision.to_string(),
                                },
                        },
                    )
                } else {
                    WorkspaceDocumentDiagnosticReport::Full(WorkspaceFullDocumentDiagnosticReport {
                        uri,
                        version: None,
                        full_document_diagnostic_report: FullDocumentDiagnosticReport {
                            result_id: Some(part.revision.to_string()),
                            items: diagnostics_of(&part, file)?,
                        },
                    })
                };
                reports.push(report);
            }
            Ok(reports)
        })
        .collect();
    let mut items = Vec::with_capacity(files.len());
    for part in reports {
        items.extend(part?);
    }
    let result = WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items });
    Ok(serde_json::to_value(result).unwrap_or_default())
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
