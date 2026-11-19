//! `textDocument/semanticTokens/*`: a file's tokens whole, the change since
//! the editor's last ones (`full/delta`: the tokens remembered per file by
//! the id they were sent with), or those of a range (what's on screen).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use ide::TokenKind;
use lsp_types::{
    SemanticToken, SemanticTokens, SemanticTokensDelta, SemanticTokensEdit,
    SemanticTokensFullDeltaResult, SemanticTokensRangeResult, SemanticTokensResult,
};
use rustc_hash::FxHashMap;
use syntax::TextRange;
use vfs::{FileId, LineIndex};

use super::convert;
use super::handlers::Snapshot;

/// The tokens last sent for each open file, by the id sent with them.
#[derive(Default)]
pub(super) struct SentTokens {
    next_id: AtomicU64,
    by_file: Mutex<FxHashMap<FileId, (String, Vec<SemanticToken>)>>,
}

impl SentTokens {
    /// Remembers `tokens` as sent for `file`; the id they're sent with.
    fn remember(&self, file: FileId, tokens: &[SemanticToken]) -> String {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
        self.lock().insert(file, (id.clone(), tokens.to_vec()));
        id
    }

    /// What was sent for `file` with `id`, if that's the last sent.
    fn sent(&self, file: FileId, id: &str) -> Option<Vec<SemanticToken>> {
        self.lock()
            .get(&file)
            .filter(|(sent_id, _)| sent_id == id)
            .map(|(_, tokens)| tokens.clone())
    }

    /// A closed file's tokens aren't asked for again.
    pub(super) fn forget(&self, file: FileId) {
        self.lock().remove(&file);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<FileId, (String, Vec<SemanticToken>)>> {
        self.by_file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// `semanticTokens/full`: every token, remembered for a later delta.
pub(super) fn full(snapshot: &Snapshot) -> ide::Cancellable<serde_json::Value> {
    let Some((file, tokens)) = tokens(snapshot, None)? else {
        return Ok(serde_json::Value::Null);
    };
    let id = snapshot.sent_tokens.remember(file, &tokens);
    let result = SemanticTokensResult::Tokens(SemanticTokens {
        result_id: Some(id),
        data: tokens,
    });
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// `semanticTokens/full/delta`: what changed since the tokens sent with
/// `previous` — all of them when those aren't the last sent.
pub(super) fn delta(snapshot: &Snapshot, previous: &str) -> ide::Cancellable<serde_json::Value> {
    let Some((file, tokens)) = tokens(snapshot, None)? else {
        return Ok(serde_json::Value::Null);
    };
    let sent = snapshot.sent_tokens.sent(file, previous);
    let id = snapshot.sent_tokens.remember(file, &tokens);
    let result = match sent {
        Some(sent) => SemanticTokensFullDeltaResult::TokensDelta(SemanticTokensDelta {
            result_id: Some(id),
            edits: edits(&sent, &tokens),
        }),
        None => SemanticTokensFullDeltaResult::Tokens(SemanticTokens {
            result_id: Some(id),
            data: tokens,
        }),
    };
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// `semanticTokens/range`: the tokens in `range`.
pub(super) fn range(
    snapshot: &Snapshot,
    range: lsp_types::Range,
) -> ide::Cancellable<serde_json::Value> {
    let Some((_, tokens)) = tokens(snapshot, Some(range))? else {
        return Ok(serde_json::Value::Null);
    };
    let result = SemanticTokensRangeResult::Tokens(SemanticTokens {
        result_id: None,
        data: tokens,
    });
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// The request's file's tokens (those overlapping `range`, when given),
/// encoded as LSP has them: each relative to the one before.
fn tokens(
    snapshot: &Snapshot,
    range: Option<lsp_types::Range>,
) -> ide::Cancellable<Option<(FileId, Vec<SemanticToken>)>> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(None);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(None);
    };
    let index = LineIndex::with_unit(&text, snapshot.encoding);
    let wanted = range.map(|range| {
        TextRange::new(
            convert::offset(&text, range.start, snapshot.encoding),
            convert::offset(&text, range.end, snapshot.encoding),
        )
    });
    let mut data = Vec::new();
    let (mut previous_line, mut previous_start) = (0, 0);
    for token in snapshot.analysis.semantic_tokens(file)? {
        let overlaps = wanted
            .is_none_or(|wanted| token.range.start < wanted.end && wanted.start < token.range.end);
        if !overlaps {
            continue;
        }
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
        data.push(SemanticToken {
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
    Ok(Some((file, data)))
}

/// One edit turning `sent` into `now`: what's between their common start
/// and common end. Positions count numbers, five to a token.
fn edits(sent: &[SemanticToken], now: &[SemanticToken]) -> Vec<SemanticTokensEdit> {
    let common_start = sent
        .iter()
        .zip(now)
        .take_while(|(old, new)| old == new)
        .count();
    let common_end = sent[common_start..]
        .iter()
        .rev()
        .zip(now[common_start..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let removed = sent.len() - common_start - common_end;
    let inserted = &now[common_start..now.len() - common_end];
    if removed == 0 && inserted.is_empty() {
        return Vec::new();
    }
    vec![SemanticTokensEdit {
        start: (common_start * 5) as u32,
        delete_count: (removed * 5) as u32,
        data: Some(inserted.to_vec()),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(delta_line: u32, delta_start: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length: 3,
            token_type: 0,
            token_modifiers_bitset: 0,
        }
    }

    /// `sent` with `edits` applied, as an editor applies them.
    fn apply(sent: &[SemanticToken], edits: &[SemanticTokensEdit]) -> Vec<SemanticToken> {
        let mut tokens = sent.to_vec();
        for edit in edits.iter().rev() {
            let start = edit.start as usize / 5;
            let end = start + edit.delete_count as usize / 5;
            tokens.splice(start..end, edit.data.clone().unwrap_or_default());
        }
        tokens
    }

    #[test]
    fn a_delta_is_the_part_between_the_common_start_and_end() {
        let sent = [token(0, 1), token(1, 2), token(1, 3), token(2, 4)];
        let now = [
            token(0, 1),
            token(1, 5),
            token(0, 6),
            token(1, 3),
            token(2, 4),
        ];
        let edits = edits(&sent, &now);
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].start, edits[0].delete_count), (5, 5));
        assert_eq!(apply(&sent, &edits), now);
    }

    #[test]
    fn nothing_changed_is_no_edit_and_everything_changed_is_one() {
        let sent = [token(0, 1), token(1, 2)];
        assert!(edits(&sent, &sent).is_empty());
        let now = [token(3, 3)];
        assert_eq!(apply(&sent, &edits(&sent, &now)), now);
        assert_eq!(apply(&[], &edits(&[], &now)), now);
        assert_eq!(
            apply(&sent, &edits(&sent, &[])),
            Vec::<SemanticToken>::new()
        );
    }
}
