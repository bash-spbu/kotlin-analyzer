//! Requests for writing code: completion, signature help, references,
//! rename, semantic tokens, folding.

use std::sync::Arc;

use ide::{CompletionKind, FoldKind, TokenKind};
use lsp_server::Request;
use lsp_types::request::{
    Completion, FoldingRangeRequest, PrepareRenameRequest, References, Rename, Request as _,
    SemanticTokensFullRequest, SignatureHelpRequest,
};
use vfs::{FileId, LineIndex};

use super::convert;
use super::handlers::Snapshot;

const METHODS: [&str; 7] = [
    Completion::METHOD,
    SignatureHelpRequest::METHOD,
    References::METHOD,
    PrepareRenameRequest::METHOD,
    Rename::METHOD,
    SemanticTokensFullRequest::METHOD,
    FoldingRangeRequest::METHOD,
];

pub(super) fn handles(method: &str) -> bool {
    METHODS.contains(&method)
}

type Answer = Result<ide::Cancellable<serde_json::Value>, String>;

pub(super) fn handle(snapshot: &Snapshot, request: Request) -> Answer {
    match request.method.as_str() {
        Completion::METHOD => {
            params::<Completion>(request).map(|params| completion(snapshot, params))
        }
        SignatureHelpRequest::METHOD => {
            params::<SignatureHelpRequest>(request).map(|params| signature_help(snapshot, params))
        }
        References::METHOD => {
            params::<References>(request).map(|params| references(snapshot, params))
        }
        PrepareRenameRequest::METHOD => {
            params::<PrepareRenameRequest>(request).map(|params| prepare_rename(snapshot, params))
        }
        Rename::METHOD => params::<Rename>(request).and_then(|params| rename(snapshot, params)),
        SemanticTokensFullRequest::METHOD => {
            params::<SemanticTokensFullRequest>(request).map(|_| semantic_tokens(snapshot))
        }
        FoldingRangeRequest::METHOD => {
            params::<FoldingRangeRequest>(request).map(|_| folding(snapshot))
        }
        method => Err(format!("unknown request {method}")),
    }
}

fn params<R: lsp_types::request::Request>(request: Request) -> Result<R::Params, String> {
    serde_json::from_value(request.params).map_err(|error| error.to_string())
}

/// The request's file (once loaded) with its text and line index.
fn document(snapshot: &Snapshot) -> ide::Cancellable<Option<(FileId, Arc<str>, LineIndex)>> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(None);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(None);
    };
    let index = LineIndex::new(&text);
    Ok(Some((file, text, index)))
}

fn completion(
    snapshot: &Snapshot,
    params: lsp_types::CompletionParams,
) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, index)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.text_document_position.position);
    let completions = snapshot.analysis.completions(file, offset)?;
    let items: Vec<lsp_types::CompletionItem> = completions
        .items
        .iter()
        .enumerate()
        .map(|(position, item)| lsp_types::CompletionItem {
            label: item.label.clone(),
            kind: Some(completion_kind(item.kind)),
            detail: item.detail.clone(),
            sort_text: Some(format!("{position:05}")),
            filter_text: Some(item.label.clone()),
            text_edit: Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                range: convert::range(&index, &text, item.replace),
                new_text: item.insert_text().into_owned(),
            })),
            additional_text_edits: item.import_edit.as_ref().map(|edit| {
                vec![lsp_types::TextEdit {
                    range: convert::range(&index, &text, edit.range),
                    new_text: edit.new_text.clone(),
                }]
            }),
            ..lsp_types::CompletionItem::default()
        })
        .collect();
    let list = lsp_types::CompletionList {
        is_incomplete: completions.incomplete,
        items,
    };
    Ok(serde_json::to_value(list).unwrap_or_default())
}

fn completion_kind(kind: CompletionKind) -> lsp_types::CompletionItemKind {
    use lsp_types::CompletionItemKind as Lsp;
    match kind {
        CompletionKind::Variable => Lsp::VARIABLE,
        CompletionKind::Function => Lsp::FUNCTION,
        CompletionKind::Method => Lsp::METHOD,
        CompletionKind::Property => Lsp::PROPERTY,
        CompletionKind::Field => Lsp::FIELD,
        CompletionKind::Class => Lsp::CLASS,
        CompletionKind::Interface => Lsp::INTERFACE,
        CompletionKind::Enum => Lsp::ENUM,
        CompletionKind::EnumMember => Lsp::ENUM_MEMBER,
        CompletionKind::Object => Lsp::MODULE,
        CompletionKind::TypeAlias => Lsp::CLASS,
        CompletionKind::Package => Lsp::MODULE,
    }
}

fn signature_help(
    snapshot: &Snapshot,
    params: lsp_types::SignatureHelpParams,
) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, _)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.text_document_position_params.position);
    let Some(help) = snapshot.analysis.signature_help(file, offset)? else {
        return Ok(serde_json::Value::Null);
    };
    let utf16 = |text: &str| text.encode_utf16().count() as u32;
    let signatures = help
        .signatures
        .iter()
        .map(|signature| lsp_types::SignatureInformation {
            label: signature.label.clone(),
            documentation: None,
            parameters: Some(
                signature
                    .parameters
                    .iter()
                    .map(|(start, end)| lsp_types::ParameterInformation {
                        label: lsp_types::ParameterLabel::LabelOffsets([
                            utf16(&signature.label[..*start]),
                            utf16(&signature.label[..*end]),
                        ]),
                        documentation: None,
                    })
                    .collect(),
            ),
            active_parameter: None,
        })
        .collect();
    let help = lsp_types::SignatureHelp {
        signatures,
        active_signature: Some(help.active_signature as u32),
        active_parameter: help.active_parameter.map(|active| active as u32),
    };
    Ok(serde_json::to_value(help).unwrap_or_default())
}

/// A file range as an LSP location.
fn location(
    snapshot: &Snapshot,
    range: ide::FileRange,
) -> ide::Cancellable<Option<lsp_types::Location>> {
    let Some(text) = snapshot.analysis.file_text(range.file)? else {
        return Ok(None);
    };
    let Some(uri) = snapshot
        .paths
        .get(range.file.index())
        .and_then(|path| convert::uri_of(path))
    else {
        return Ok(None);
    };
    let index = LineIndex::new(&text);
    Ok(Some(lsp_types::Location {
        uri,
        range: convert::range(&index, &text, range.range),
    }))
}

fn references(
    snapshot: &Snapshot,
    params: lsp_types::ReferenceParams,
) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, _)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.text_document_position.position);
    let mut locations = Vec::new();
    for found in snapshot
        .analysis
        .references(file, offset, params.context.include_declaration)?
    {
        locations.extend(location(snapshot, found)?);
    }
    Ok(serde_json::to_value(locations).unwrap_or_default())
}

fn prepare_rename(
    snapshot: &Snapshot,
    params: lsp_types::TextDocumentPositionParams,
) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, index)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let offset = convert::offset(&text, params.position);
    match snapshot.analysis.prepare_rename(file, offset)? {
        Ok(range) => {
            Ok(serde_json::to_value(convert::range(&index, &text, range)).unwrap_or_default())
        }
        Err(_) => Ok(serde_json::Value::Null),
    }
}

fn rename(snapshot: &Snapshot, params: lsp_types::RenameParams) -> Answer {
    let answer = (|| -> ide::Cancellable<Result<serde_json::Value, String>> {
        let Some((file, text, _)) = document(snapshot)? else {
            return Ok(Ok(serde_json::Value::Null));
        };
        let offset = convert::offset(&text, params.text_document_position.position);
        let change = match snapshot.analysis.rename(file, offset, &params.new_name)? {
            Ok(change) => change,
            Err(error) => return Ok(Err(error.to_string())),
        };
        let mut document_edits = Vec::new();
        for (file, edits) in change.edits {
            let (Some(text), Some(uri)) = (
                snapshot.analysis.file_text(file)?,
                snapshot
                    .paths
                    .get(file.index())
                    .and_then(|path| convert::uri_of(path)),
            ) else {
                continue;
            };
            let index = LineIndex::new(&text);
            document_edits.push(lsp_types::TextDocumentEdit {
                text_document: lsp_types::OptionalVersionedTextDocumentIdentifier {
                    uri,
                    version: None,
                },
                edits: edits
                    .into_iter()
                    .map(|edit| {
                        lsp_types::OneOf::Left(lsp_types::TextEdit {
                            range: convert::range(&index, &text, edit.range),
                            new_text: edit.new_text,
                        })
                    })
                    .collect(),
            });
        }
        let edit = lsp_types::WorkspaceEdit {
            document_changes: Some(lsp_types::DocumentChanges::Edits(document_edits)),
            ..lsp_types::WorkspaceEdit::default()
        };
        Ok(Ok(serde_json::to_value(edit).unwrap_or_default()))
    })();
    match answer {
        Ok(Ok(value)) => Ok(Ok(value)),
        Ok(Err(message)) => Err(message),
        Err(cancelled) => Ok(Err(cancelled)),
    }
}

pub(super) fn semantic_tokens_legend() -> lsp_types::SemanticTokensLegend {
    use lsp_types::SemanticTokenType as Type;
    let token_type = |kind: TokenKind| match kind {
        TokenKind::Namespace => Type::NAMESPACE,
        TokenKind::Class => Type::CLASS,
        TokenKind::Interface => Type::INTERFACE,
        TokenKind::Enum => Type::ENUM,
        TokenKind::TypeParameter => Type::TYPE_PARAMETER,
        TokenKind::Parameter => Type::PARAMETER,
        TokenKind::Variable => Type::VARIABLE,
        TokenKind::Property => Type::PROPERTY,
        TokenKind::EnumMember => Type::ENUM_MEMBER,
        TokenKind::Function => Type::FUNCTION,
        TokenKind::Method => Type::METHOD,
    };
    lsp_types::SemanticTokensLegend {
        token_types: TokenKind::ALL.into_iter().map(token_type).collect(),
        token_modifiers: ide::TokenModifiers::NAMES
            .iter()
            .map(|name| lsp_types::SemanticTokenModifier::new(name))
            .collect(),
    }
}

fn semantic_tokens(snapshot: &Snapshot) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, index)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let mut data = Vec::new();
    let (mut previous_line, mut previous_start) = (0, 0);
    for token in snapshot.analysis.semantic_tokens(file)? {
        let start = convert::position(&index, &text, token.range.start as usize);
        let end = convert::position(&index, &text, token.range.end as usize);
        if start.line != end.line {
            continue;
        }
        let delta_line = start.line - previous_line;
        let delta_start = if delta_line == 0 {
            start.character - previous_start
        } else {
            start.character
        };
        data.push(lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length: end.character - start.character,
            token_type: TokenKind::ALL
                .iter()
                .position(|kind| *kind == token.kind)
                .unwrap_or(0) as u32,
            token_modifiers_bitset: token.modifiers.0,
        });
        previous_line = start.line;
        previous_start = start.character;
    }
    let tokens = lsp_types::SemanticTokens {
        result_id: None,
        data,
    };
    Ok(serde_json::to_value(tokens).unwrap_or_default())
}

fn folding(snapshot: &Snapshot) -> ide::Cancellable<serde_json::Value> {
    let Some((file, text, index)) = document(snapshot)? else {
        return Ok(serde_json::Value::Null);
    };
    let folds: Vec<lsp_types::FoldingRange> = snapshot
        .analysis
        .folding_ranges(file)?
        .into_iter()
        .map(|fold| {
            let start = convert::position(&index, &text, fold.range.start as usize);
            let end = convert::position(&index, &text, fold.range.end as usize);
            lsp_types::FoldingRange {
                start_line: start.line,
                end_line: end.line,
                kind: Some(match fold.kind {
                    FoldKind::Region => lsp_types::FoldingRangeKind::Region,
                    FoldKind::Imports => lsp_types::FoldingRangeKind::Imports,
                    FoldKind::Comment => lsp_types::FoldingRangeKind::Comment,
                }),
                ..lsp_types::FoldingRange::default()
            }
        })
        .collect();
    Ok(serde_json::to_value(folds).unwrap_or_default())
}
