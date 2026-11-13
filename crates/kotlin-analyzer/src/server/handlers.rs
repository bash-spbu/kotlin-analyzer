//! Requests answered on a worker thread, against a database snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use ide::{Analysis, Cancellable, NavigationTarget, SymbolKind};
use lsp_server::{ErrorCode, Request, Response};
use lsp_types::request::{
    DocumentSymbolRequest, GotoDefinition, Request as _, WorkspaceSymbolRequest,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbolResponse, GotoDefinitionResponse, Location,
    WorkspaceSymbolResponse,
};
use vfs::{FileId, LineIndex};

use super::convert;

/// What a request needs from the main thread.
pub(super) struct Snapshot {
    pub(super) analysis: Analysis,
    /// Paths by file id.
    pub(super) paths: Arc<Vec<PathBuf>>,
    /// Whether the workspace has finished loading.
    pub(super) loaded: bool,
    /// The file the request is about, when it names one the server knows.
    pub(super) request_file: Option<FileId>,
}

/// How many results `workspace/symbol` returns at most.
const WORKSPACE_SYMBOL_LIMIT: usize = 128;

pub(super) fn handle(snapshot: Snapshot, request: Request) -> Response {
    let id = request.id.clone();
    let result = match request.method.as_str() {
        GotoDefinition::METHOD => {
            params::<GotoDefinition>(request).map(|params| definition(&snapshot, params))
        }
        DocumentSymbolRequest::METHOD => {
            params::<DocumentSymbolRequest>(request).map(|_| document_symbols(&snapshot))
        }
        WorkspaceSymbolRequest::METHOD => params::<WorkspaceSymbolRequest>(request)
            .map(|params| workspace_symbols(&snapshot, &params.query)),
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
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(serde_json::Value::Null);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.text_document_position_params.position);
    let targets = snapshot.analysis.goto_definition(file, offset)?;
    let mut locations = Vec::new();
    for target in &targets {
        if let Some(location) = location_of(snapshot, target)? {
            locations.push(location);
        }
    }
    Ok(serde_json::to_value(GotoDefinitionResponse::Array(locations)).unwrap_or_default())
}

fn location_of(snapshot: &Snapshot, target: &NavigationTarget) -> Cancellable<Option<Location>> {
    let Some(text) = snapshot.analysis.file_text(target.file)? else {
        return Ok(None);
    };
    let Some(path) = snapshot.paths.get(target.file.index()) else {
        return Ok(None);
    };
    let Some(uri) = convert::uri_of(path) else {
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

pub(super) fn syntax_diagnostics(
    analysis: &Analysis,
    file: FileId,
) -> Cancellable<Vec<Diagnostic>> {
    let Some(text) = analysis.file_text(file)? else {
        return Ok(Vec::new());
    };
    let index = LineIndex::new(&text);
    let errors = analysis.syntax_errors(file)?;
    Ok(errors
        .into_iter()
        .map(|error| Diagnostic {
            range: convert::range(&index, &text, error.range),
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(lsp_types::NumberOrString::String("syntax".to_owned())),
            source: Some("kotlin-analyzer".to_owned()),
            message: error.message,
            ..Diagnostic::default()
        })
        .collect())
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
        SymbolKind::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
    }
}
