//! Requests answered on a worker thread, against a database snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use ide::{Analysis, Cancellable, NavigationFile, NavigationTarget, SymbolKind};
use lsp_server::{ErrorCode, Request, Response};
use lsp_types::request::{
    CodeActionRequest, DocumentDiagnosticRequest, DocumentSymbolRequest, GotoDeclaration,
    GotoDefinition, GotoImplementation, GotoTypeDefinition, HoverRequest, InlayHintRequest,
    InlayHintResolveRequest, Request as _, SelectionRangeRequest, WorkspaceDiagnosticRequest,
    WorkspaceSymbolRequest,
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
    pub(super) formatting: Arc<super::formatting::FormattingConfig>,
    pub(super) code_lens_config: super::code_lens::CodeLensConfig,
    /// What a position's column counts, as agreed with the client.
    pub(super) encoding: vfs::ColumnUnit,
    /// How many times the workspace has changed: a pull diagnostics
    /// report's id.
    pub(super) revision: u64,
    /// The semantic tokens last sent per file, for deltas.
    pub(super) sent_tokens: Arc<super::semantic_tokens::SentTokens>,
}

impl Snapshot {
    /// The same snapshot for another thread.
    pub(super) fn with_analysis_clone(&self) -> Snapshot {
        Snapshot {
            analysis: self.analysis.clone(),
            paths: self.paths.clone(),
            loaded: self.loaded,
            request_file: self.request_file,
            diagnostics_config: self.diagnostics_config.clone(),
            formatting: self.formatting.clone(),
            code_lens_config: self.code_lens_config,
            sent_tokens: self.sent_tokens.clone(),
            encoding: self.encoding,
            revision: self.revision,
        }
    }
}

/// How many results `workspace/symbol` returns at most.
const WORKSPACE_SYMBOL_LIMIT: usize = 128;

pub(super) fn handle(snapshot: Snapshot, request: Request) -> Response {
    #[cfg(test)]
    if request.method == "kotlin-analyzer/testPanic" {
        panic!("a test panic about `{}`", request.params["secret"]);
    }
    #[cfg(test)]
    if request.method == "kotlin-analyzer/testSlow" {
        std::thread::sleep(std::time::Duration::from_millis(300));
        return Response::new_ok(request.id, "slow");
    }
    let id = request.id.clone();
    let result =
        match request.method.as_str() {
            GotoDefinition::METHOD => {
                params::<GotoDefinition>(request).map(|params| definition(&snapshot, params))
            }
            GotoImplementation::METHOD => params::<GotoImplementation>(request)
                .map(|params| implementation(&snapshot, params)),
            GotoDeclaration::METHOD => {
                params::<GotoDeclaration>(request).map(|params| declaration(&snapshot, params))
            }
            GotoTypeDefinition::METHOD => params::<GotoTypeDefinition>(request)
                .map(|params| type_definition(&snapshot, params)),
            HoverRequest::METHOD => {
                params::<HoverRequest>(request).map(|params| hover(&snapshot, params))
            }
            CodeActionRequest::METHOD => {
                params::<CodeActionRequest>(request).map(|params| code_actions(&snapshot, params))
            }
            InlayHintRequest::METHOD => {
                params::<InlayHintRequest>(request).map(|params| inlay_hints(&snapshot, params))
            }
            InlayHintResolveRequest::METHOD => params::<InlayHintResolveRequest>(request)
                .map(|hint| resolve_inlay_hint(&snapshot, hint)),
            DocumentSymbolRequest::METHOD => {
                params::<DocumentSymbolRequest>(request).map(|_| document_symbols(&snapshot))
            }
            WorkspaceSymbolRequest::METHOD => params::<WorkspaceSymbolRequest>(request)
                .map(|params| workspace_symbols(&snapshot, &params.query)),
            SelectionRangeRequest::METHOD => params::<SelectionRangeRequest>(request)
                .map(|params| selection_ranges(&snapshot, &params.positions)),
            method if editing::handles(method) => editing::handle(&snapshot, request),
            DocumentDiagnosticRequest::METHOD => params::<DocumentDiagnosticRequest>(request)
                .map(|params| super::pull_diagnostics::document(&snapshot, params)),
            WorkspaceDiagnosticRequest::METHOD => params::<WorkspaceDiagnosticRequest>(request)
                .map(|params| super::pull_diagnostics::workspace(&snapshot, params, None)),
            method if super::code_lens::METHODS.contains(&method) => {
                Ok(super::code_lens::handle(&snapshot, method, &request.params))
            }
            method if super::hierarchy::METHODS.contains(&method) => {
                Ok(super::hierarchy::handle(&snapshot, method, &request.params))
            }
            method => {
                return Response::new_err(
                    id,
                    ErrorCode::MethodNotFound as i32,
                    format!("unknown request {method}"),
                );
            }
        };
    response(id, result)
}

/// The response to a request with `result`: its value, "content modified"
/// when an edit cancelled it, or its parameters' error.
pub(super) fn response(
    id: lsp_server::RequestId,
    result: Result<Cancellable<serde_json::Value>, String>,
) -> Response {
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

/// For each position, the ranges "expand selection" grows through, as LSP
/// nests them (the innermost first, each with the next as its parent); a
/// position in a file the server doesn't know gets just itself.
fn selection_ranges(
    snapshot: &Snapshot,
    positions: &[lsp_types::Position],
) -> Cancellable<serde_json::Value> {
    let empty_at = |position: lsp_types::Position| lsp_types::SelectionRange {
        range: lsp_types::Range::new(position, position),
        parent: None,
    };
    let text = match snapshot.request_file {
        Some(file) => snapshot.analysis.file_text(file)?,
        None => None,
    };
    let (Some(file), Some(text)) = (snapshot.request_file, text) else {
        let answer: Vec<_> = positions.iter().copied().map(empty_at).collect();
        return Ok(serde_json::to_value(answer).unwrap_or_default());
    };
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    let offsets: Vec<usize> = positions
        .iter()
        .map(|position| convert::offset(&text, *position, snapshot.encoding))
        .collect();
    let all_ranges = snapshot.analysis.selection_ranges(file, &offsets)?;
    let answer: Vec<lsp_types::SelectionRange> = positions
        .iter()
        .zip(all_ranges)
        .map(|(position, ranges)| {
            let outermost_first = ranges.iter().rev();
            outermost_first
                .fold(None, |parent, range| {
                    Some(lsp_types::SelectionRange {
                        range: convert::range(&index, &text, *range),
                        parent: parent.map(Box::new),
                    })
                })
                .unwrap_or_else(|| empty_at(*position))
        })
        .collect();
    Ok(serde_json::to_value(answer).unwrap_or_default())
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

fn declaration(
    snapshot: &Snapshot,
    params: lsp_types::request::GotoDeclarationParams,
) -> Cancellable<serde_json::Value> {
    navigation(
        snapshot,
        params.text_document_position_params.position,
        |file, offset| snapshot.analysis.goto_declaration(file, offset),
    )
}

fn type_definition(
    snapshot: &Snapshot,
    params: lsp_types::request::GotoTypeDefinitionParams,
) -> Cancellable<serde_json::Value> {
    navigation(
        snapshot,
        params.text_document_position_params.position,
        |file, offset| snapshot.analysis.goto_type_definition(file, offset),
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
    let offset = convert::offset(&text, position, snapshot.encoding);
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
    let offset = convert::offset(
        &text,
        params.text_document_position_params.position,
        snapshot.encoding,
    );
    let Some(result) = snapshot.analysis.hover(file, offset)? else {
        return Ok(serde_json::Value::Null);
    };
    let index = LineIndex::with_unit(&text, snapshot.encoding);
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
        convert::offset(&text, params.range.start, snapshot.encoding),
        convert::offset(&text, params.range.end, snapshot.encoding),
    );
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    let uri = snapshot
        .paths
        .get(file.index())
        .and_then(|path| convert::uri_of(path));
    let mut hints: Vec<lsp_types::InlayHint> = Vec::new();
    for hint in snapshot.analysis.inlay_hints(file, Some(range))? {
        let mut parts = Vec::with_capacity(hint.label.len());
        for part in &hint.label {
            let location = match &part.target {
                Some(ide::HintTarget::Declaration(target)) => location_of(snapshot, target)?,
                Some(ide::HintTarget::LibraryDeclaration) | None => None,
            };
            parts.push(lsp_types::InlayHintLabelPart {
                value: part.text.clone(),
                location,
                ..lsp_types::InlayHintLabelPart::default()
            });
        }
        // A library class's place is found (its source read) only when the
        // editor asks: `inlayHint/resolve`, with what tells the hint again.
        let resolvable = hint
            .label
            .iter()
            .any(|part| part.target == Some(ide::HintTarget::LibraryDeclaration));
        let data = match (&uri, resolvable) {
            (Some(uri), true) => Some(serde_json::json!({
                "textDocument": { "uri": uri },
                "offset": hint.offset,
            })),
            _ => None,
        };
        hints.push(lsp_types::InlayHint {
            position: convert::position(&index, &text, hint.offset as usize),
            label: lsp_types::InlayHintLabel::LabelParts(parts),
            kind: Some(lsp_types::InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data,
        });
    }
    Ok(serde_json::to_value(hints).unwrap_or_default())
}

/// `workspace/willRenameFiles`: the edits that keep the code right once
/// `moves` happen (made to the files as they are before).
pub(super) fn will_rename_files(
    snapshot: &Snapshot,
    id: lsp_server::RequestId,
    moves: &[ide::FileMove],
) -> Response {
    if !snapshot.loaded {
        return Response::new_ok(id, serde_json::Value::Null);
    }
    let edit = snapshot
        .analysis
        .will_rename_files(moves)
        .and_then(|change| {
            let nothing = change.edits.is_empty();
            let edit = super::editing::workspace_edit(snapshot, change)?;
            Ok((!nothing).then_some(edit))
        });
    match edit {
        Ok(edit) => Response::new_ok(id, edit),
        Err(_cancelled) => Response::new_err(
            id,
            ErrorCode::ContentModified as i32,
            "content modified".to_owned(),
        ),
    }
}

/// `inlayHint/resolve`: the hint with each library class name linked to
/// its declaration in the library's sources (when they're there).
fn resolve_inlay_hint(
    snapshot: &Snapshot,
    mut hint: lsp_types::InlayHint,
) -> Cancellable<serde_json::Value> {
    let offset = hint
        .data
        .as_ref()
        .and_then(|data| data.get("offset"))
        .and_then(serde_json::Value::as_u64);
    let file = snapshot.request_file.filter(|_| snapshot.loaded);
    let (Some(file), Some(offset), lsp_types::InlayHintLabel::LabelParts(parts)) =
        (file, offset, &mut hint.label)
    else {
        return Ok(serde_json::to_value(hint).unwrap_or_default());
    };
    let targets = snapshot.analysis.resolve_inlay_hint(file, offset as u32)?;
    // The hint is the one sent only if it still has as many parts.
    if targets.len() == parts.len() {
        for (part, target) in parts.iter_mut().zip(targets) {
            if part.location.is_some() {
                continue;
            }
            if let Some(target) = target {
                part.location = location_of(snapshot, &target)?;
            }
        }
    }
    Ok(serde_json::to_value(hint).unwrap_or_default())
}

fn location_of(snapshot: &Snapshot, target: &NavigationTarget) -> Cancellable<Option<Location>> {
    Ok(target_ranges(snapshot, target)?.map(|ranges| Location {
        uri: ranges.uri,
        range: ranges.focus,
    }))
}

/// Where a navigation target is, as LSP has it.
pub(super) struct TargetRanges {
    pub(super) uri: lsp_types::Uri,
    /// The whole declaration.
    pub(super) full: lsp_types::Range,
    /// Its name.
    pub(super) focus: lsp_types::Range,
}

pub(super) fn target_ranges(
    snapshot: &Snapshot,
    target: &NavigationTarget,
) -> Cancellable<Option<TargetRanges>> {
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
            ..
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
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    Ok(Some(TargetRanges {
        uri,
        full: convert::range(&index, &text, target.full_range),
        focus: convert::range(&index, &text, target.focus_range),
    }))
}

fn document_symbols(snapshot: &Snapshot) -> Cancellable<serde_json::Value> {
    let Some(file) = snapshot.request_file else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let index = LineIndex::with_unit(&text, snapshot.encoding);
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
    unit: vfs::ColumnUnit,
) -> Cancellable<Vec<Diagnostic>> {
    let Some(text) = analysis.file_text(file)? else {
        return Ok(Vec::new());
    };
    let index = LineIndex::with_unit(&text, unit);
    Ok(analysis
        .diagnostics(file, config)?
        .into_iter()
        .map(|diagnostic| to_lsp(&index, &text, &diagnostic))
        .collect())
}

fn to_lsp(index: &LineIndex, text: &str, diagnostic: &ide::Diagnostic) -> Diagnostic {
    let (severity, tags) = match diagnostic.severity {
        ide::Severity::Error => (DiagnosticSeverity::ERROR, None),
        ide::Severity::Warning | ide::Severity::Lint => (DiagnosticSeverity::WARNING, None),
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
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    let start = convert::offset(&text, params.range.start, snapshot.encoding) as u32;
    let end = convert::offset(&text, params.range.end, snapshot.encoding) as u32;
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

pub(super) fn symbol_kind(kind: SymbolKind) -> lsp_types::SymbolKind {
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
