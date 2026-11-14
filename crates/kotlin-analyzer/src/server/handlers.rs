//! Requests answered on a worker thread, against a database snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use ide::{Analysis, Cancellable, NavigationFile, NavigationTarget, SymbolKind};
use lsp_server::{ErrorCode, Request, Response};
use lsp_types::request::{
    CodeActionRequest, DocumentSymbolRequest, GotoDefinition, GotoImplementation, HoverRequest,
    InlayHintRequest, Request as _, WorkspaceSymbolRequest,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbolResponse, GotoDefinitionResponse, Location,
    WorkspaceSymbolResponse,
};
use vfs::{FileId, LineIndex};

use super::{convert, editing, library_sources};

/// What a request needs from the main thread.
pub(super) struct Snapshot {
    pub(super) analysis: Analysis,
    /// Paths by file id.
    pub(super) paths: Arc<Vec<PathBuf>>,
    /// Whether the workspace has finished loading.
    pub(super) loaded: bool,
    /// The file the request is about, when it names one the server knows.
    pub(super) request_file: Option<FileId>,
    pub(super) diagnostics_config: Arc<ide::DiagnosticsConfig>,
}

/// How many results `workspace/symbol` returns at most.
const WORKSPACE_SYMBOL_LIMIT: usize = 128;

pub(super) fn handle(snapshot: Snapshot, request: Request) -> Response {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        GotoDefinition::METHOD => {
            params::<GotoDefinition>(request).map(|params| definition(&snapshot, params))
        }
        GotoImplementation::METHOD => {
            params::<GotoImplementation>(request).map(|params| implementation(&snapshot, params))
        }
        HoverRequest::METHOD => {
            params::<HoverRequest>(request).map(|params| hover(&snapshot, params))
        }
        CodeActionRequest::METHOD => {
            params::<CodeActionRequest>(request).map(|params| code_actions(&snapshot, params))
        }
        InlayHintRequest::METHOD => {
            params::<InlayHintRequest>(request).map(|params| inlay_hints(&snapshot, params))
        }
        DocumentSymbolRequest::METHOD => {
            params::<DocumentSymbolRequest>(request).map(|_| document_symbols(&snapshot))
        }
        WorkspaceSymbolRequest::METHOD => params::<WorkspaceSymbolRequest>(request)
            .map(|params| workspace_symbols(&snapshot, &params.query)),
        method if editing::handles(method) => editing::handle(&snapshot, request),
        method => {
            return Response::new_err(
                id,
                ErrorCode::MethodNotFound as i32,
                format!("unknown request {method}"),
            );
        }
    };
    match result {
        Ok(Ok(value)) => Response::new_ok(id, value),
        Ok(Err(_cancelled)) => Response::new_err(
            id,
            ErrorCode::ContentModified as i32,
            "content modified".to_owned(),
        ),
        Err(error) => Response::new_err(id, ErrorCode::InvalidParams as i32, error),
    }
}

fn params<R: lsp_types::request::Request>(request: Request) -> Result<R::Params, String> {
    serde_json::from_value(request.params).map_err(|error| error.to_string())
}

fn definition(
    snapshot: &Snapshot,
    params: lsp_types::GotoDefinitionParams,
) -> Cancellable<serde_json::Value> {
    navigation(
        snapshot,
        params.text_document_position_params.position,
        |file, offset| snapshot.analysis.goto_definition(file, offset),
    )
}

fn implementation(
    snapshot: &Snapshot,
    params: lsp_types::request::GotoImplementationParams,
) -> Cancellable<serde_json::Value> {
    navigation(
        snapshot,
        params.text_document_position_params.position,
        |file, offset| snapshot.analysis.goto_implementation(file, offset),
    )
}

/// The locations `targets_at` gives for the request's file at `position`.
fn navigation(
    snapshot: &Snapshot,
    position: lsp_types::Position,
    targets_at: impl Fn(FileId, usize) -> Cancellable<Vec<ide::NavigationTarget>>,
) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, position);
    let targets = targets_at(file, offset)?;
    let mut locations = Vec::new();
    for target in &targets {
        if let Some(location) = location_of(snapshot, target)? {
            locations.push(location);
        }
    }
    Ok(serde_json::to_value(GotoDefinitionResponse::Array(locations)).unwrap_or_default())
}

fn hover(snapshot: &Snapshot, params: lsp_types::HoverParams) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.text_document_position_params.position);
    let Some(result) = snapshot.analysis.hover(file, offset)? else {
        return Ok(serde_json::Value::Null);
    };
    let index = LineIndex::new(&text);
    let hover = lsp_types::Hover {
        contents: lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: result.markdown,
        }),
        range: Some(convert::range(&index, &text, result.range)),
    };
    Ok(serde_json::to_value(hover).unwrap_or_default())
}

fn inlay_hints(
    snapshot: &Snapshot,
    params: lsp_types::InlayHintParams,
) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let range = syntax::TextRange::new(
        convert::offset(&text, params.range.start),
        convert::offset(&text, params.range.end),
    );
    let index = LineIndex::new(&text);
    let hints: Vec<lsp_types::InlayHint> = snapshot
        .analysis
        .inlay_hints(file, Some(range))?
        .into_iter()
        .map(|hint| lsp_types::InlayHint {
            position: convert::position(&index, &text, hint.offset as usize),
            label: lsp_types::InlayHintLabel::String(hint.label),
            kind: Some(lsp_types::InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        })
        .collect();
    Ok(serde_json::to_value(hints).unwrap_or_default())
}

fn location_of(snapshot: &Snapshot, target: &NavigationTarget) -> Cancellable<Option<Location>> {
    let (path, text) = match &target.file {
        NavigationFile::Workspace(file) => {
            let Some(text) = snapshot.analysis.file_text(*file)? else {
                return Ok(None);
            };
            let Some(path) = snapshot.paths.get(file.index()).cloned() else {
                return Ok(None);
            };
            (path, text)
        }
        NavigationFile::Library {
            archive,
            entry,
            text,
        } => {
            let Some(path) = library_sources::materialize(archive, entry, text) else {
                return Ok(None);
            };
            (path, text.clone())
        }
    };
    let Some(uri) = convert::uri_of(&path) else {
        return Ok(None);
    };
    let index = LineIndex::new(&text);
    Ok(Some(Location {
        uri,
        range: convert::range(&index, &text, target.focus_range),
    }))
}

fn document_symbols(snapshot: &Snapshot) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let index = LineIndex::new(&text);
    let symbols = snapshot.analysis.document_symbols(file)?;
    #[allow(deprecated)]
    fn convert_symbol(
        index: &LineIndex,
        text: &str,
        symbol: ide::DocumentSymbol,
    ) -> lsp_types::DocumentSymbol {
        lsp_types::DocumentSymbol {
            name: symbol.name,
            detail: symbol.detail,
            kind: symbol_kind(symbol.kind),
            tags: symbol
                .deprecated
                .then(|| vec![lsp_types::SymbolTag::DEPRECATED]),
            deprecated: None,
            range: convert::range(index, text, symbol.range),
            selection_range: convert::range(index, text, symbol.selection_range),
            children: Some(
                symbol
                    .children
                    .into_iter()
                    .map(|child| convert_symbol(index, text, child))
                    .collect(),
            ),
        }
    }
    let symbols: Vec<lsp_types::DocumentSymbol> = symbols
        .into_iter()
        .map(|symbol| convert_symbol(&index, &text, symbol))
        .collect();
    Ok(serde_json::to_value(DocumentSymbolResponse::Nested(symbols)).unwrap_or_default())
}

fn workspace_symbols(snapshot: &Snapshot, query: &str) -> Cancellable<serde_json::Value> {
    if !snapshot.loaded {
        return Ok(serde_json::Value::Null);
    }
    let targets = snapshot
        .analysis
        .workspace_symbols(query, WORKSPACE_SYMBOL_LIMIT)?;
    let mut symbols = Vec::new();
    for target in &targets {
        let Some(location) = location_of(snapshot, target)? else {
            continue;
        };
        symbols.push(lsp_types::WorkspaceSymbol {
            name: target.name.clone(),
            kind: symbol_kind(target.kind),
            tags: None,
            container_name: target.container.clone(),
            location: lsp_types::OneOf::Left(location),
            data: None,
        });
    }
    Ok(serde_json::to_value(WorkspaceSymbolResponse::Nested(symbols)).unwrap_or_default())
}

/// A file's diagnostics, as LSP has them.
pub(super) fn diagnostics(
    analysis: &Analysis,
    file: FileId,
    config: &ide::DiagnosticsConfig,
) -> Cancellable<Vec<Diagnostic>> {
    let Some(text) = analysis.file_text(file)? else {
        return Ok(Vec::new());
    };
    let index = LineIndex::new(&text);
    Ok(analysis
        .diagnostics(file, config)?
        .into_iter()
        .map(|diagnostic| to_lsp(&index, &text, &diagnostic))
        .collect())
}

fn to_lsp(index: &LineIndex, text: &str, diagnostic: &ide::Diagnostic) -> Diagnostic {
    let (severity, tags) = match diagnostic.check.severity() {
        ide::Severity::Error => (DiagnosticSeverity::ERROR, None),
        ide::Severity::Warning => (DiagnosticSeverity::WARNING, None),
        ide::Severity::Unnecessary => (
            DiagnosticSeverity::HINT,
            Some(vec![lsp_types::DiagnosticTag::UNNECESSARY]),
        ),
    };
    Diagnostic {
        range: convert::range(index, text, diagnostic.range),
        severity: Some(severity),
        code: Some(lsp_types::NumberOrString::String(
            diagnostic.check.name().to_owned(),
        )),
        source: Some("kotlin-analyzer".to_owned()),
        message: diagnostic.message.clone(),
        tags,
        ..Diagnostic::default()
    }
}

/// The fixes of the diagnostics overlapping the requested range.
fn code_actions(
    snapshot: &Snapshot,
    params: lsp_types::CodeActionParams,
) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let index = LineIndex::new(&text);
    let start = convert::offset(&text, params.range.start) as u32;
    let end = convert::offset(&text, params.range.end) as u32;
    let mut actions = Vec::new();
    for diagnostic in snapshot
        .analysis
        .diagnostics(file, &snapshot.diagnostics_config)?
    {
        let overlaps = diagnostic.range.start <= end && start <= diagnostic.range.end;
        if !overlaps {
            continue;
        }
        let lsp_diagnostic = to_lsp(&index, &text, &diagnostic);
        for fix in diagnostic.fixes {
            let edits = fix
                .edits
                .iter()
                .map(|edit| {
                    lsp_types::OneOf::Left(lsp_types::TextEdit {
                        range: convert::range(&index, &text, edit.range),
                        new_text: edit.new_text.clone(),
                    })
                })
                .collect();
            let document_edit = lsp_types::TextDocumentEdit {
                text_document: lsp_types::OptionalVersionedTextDocumentIdentifier {
                    uri: params.text_document.uri.clone(),
                    version: None,
                },
                edits,
            };
            actions.push(lsp_types::CodeActionOrCommand::CodeAction(
                lsp_types::CodeAction {
                    title: fix.label,
                    kind: Some(lsp_types::CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lsp_diagnostic.clone()]),
                    edit: Some(lsp_types::WorkspaceEdit {
                        document_changes: Some(lsp_types::DocumentChanges::Edits(vec![
                            document_edit,
                        ])),
                        ..lsp_types::WorkspaceEdit::default()
                    }),
                    ..lsp_types::CodeAction::default()
                },
            ));
        }
    }
    Ok(serde_json::to_value(actions).unwrap_or_default())
}

fn symbol_kind(kind: SymbolKind) -> lsp_types::SymbolKind {
    match kind {
        SymbolKind::Class => lsp_types::SymbolKind::CLASS,
        SymbolKind::Interface | SymbolKind::Annotation => lsp_types::SymbolKind::INTERFACE,
        SymbolKind::Enum => lsp_types::SymbolKind::ENUM,
        SymbolKind::Object => lsp_types::SymbolKind::OBJECT,
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Method => lsp_types::SymbolKind::METHOD,
        SymbolKind::Property => lsp_types::SymbolKind::PROPERTY,
        SymbolKind::Field => lsp_types::SymbolKind::FIELD,
        SymbolKind::Constructor => lsp_types::SymbolKind::CONSTRUCTOR,
        SymbolKind::EnumMember => lsp_types::SymbolKind::ENUM_MEMBER,
        SymbolKind::Variable => lsp_types::SymbolKind::VARIABLE,
        SymbolKind::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
    }
}
