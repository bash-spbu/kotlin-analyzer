//! What's in scope at a position, for completion: inference runs over the
//! file with a probe at the cursor, and the name there captures the walk's
//! state — the locals visible (with the smart casts in force), the implicit
//! receivers, and what qualifies the name (`value.`, `Class.`, `package.`).

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::ItemLocation;
use intern::Path;
use syntax::{Language, SyntaxNode};

use super::Local;
use super::declarations::Inferrer;
use super::expressions::Typed;
use super::member_scope::enclosing_member;
use crate::ty::Ty;

/// A placeholder name put where the cursor is when no name is there yet
/// (`order.|`), so the expression parses with a name to probe.
pub const PLACEHOLDER: &str = "kotlinAnalyzerCompletion";

/// The scope at a completion position.
#[derive(Clone, Debug, Default)]
pub struct CompletionScope {
    /// Locals visible there, innermost first, with their types there.
    pub locals: Vec<Local>,
    /// Implicit receivers, innermost first.
    pub receivers: Vec<Ty>,
    pub qualifier: Option<Qualifier>,
}

/// What a name after a `.` is looked up in.
#[derive(Clone, Debug)]
pub enum Qualifier {
    Value(Ty),
    Class(ItemLocation),
    Package(Path),
}

/// Where the probe is and what it captured.
#[derive(Default)]
pub(super) struct Probe {
    /// The offset (in the probed text) the name to capture contains.
    pub(super) offset: u32,
    /// Where a placeholder was inserted, and its length: positions after
    /// it map back by that much to the file's own.
    pub(super) inserted: Option<(u32, u32)>,
    pub(super) captured: Option<CompletionScope>,
}

/// The scope at `offset` in `text` — `file`'s text as the editor has it,
/// with [`PLACEHOLDER`] inserted at `offset` when `inserted`.
pub fn completion_scope(
    db: &dyn SourceDatabase,
    file: SourceFile,
    text: Arc<str>,
    offset: u32,
    inserted: bool,
) -> Option<CompletionScope> {
    if file.language(db) != Language::Kotlin {
        return None;
    }
    let parse = syntax::parse(text, Language::Kotlin);
    let mut inferrer = Inferrer::new(db, file, &parse);
    inferrer.probe = Some(Probe {
        offset,
        inserted: inserted.then_some((offset, PLACEHOLDER.len() as u32)),
        captured: None,
    });
    // Only the declaration around the cursor is walked: its scope is all a
    // completion needs, and a keystroke shouldn't infer the whole file.
    let at_cursor = parse
        .root()
        .descendant_for_byte_range(offset as usize, offset as usize);
    match at_cursor.and_then(enclosing_member) {
        Some(member) => {
            let declaration = member.declaration();
            inferrer.enter_enclosing_classes(declaration);
            inferrer.declaration(declaration);
        }
        None => inferrer.file_declarations(),
    }
    inferrer.probe.take()?.captured
}

impl Inferrer<'_> {
    /// Whether `name` is the probed name.
    pub(super) fn probed(&self, name: SyntaxNode<'_>) -> bool {
        let Some(probe) = &self.probe else {
            return false;
        };
        probe.captured.is_none()
            && name.start_byte() as u32 <= probe.offset
            && probe.offset <= name.end_byte() as u32
    }

    /// Captures the scope for the probed name, qualified by `qualifier`.
    pub(super) fn capture(&mut self, qualifier: Option<&Typed>) {
        let locals = self
            .visible_locals()
            .into_iter()
            .map(|id| {
                let mut local = self.local_declaration(id).clone();
                local.ty = self.local_type(id);
                local
            })
            .collect();
        let receivers = self
            .receivers
            .iter()
            .rev()
            .map(|receiver| receiver.ty.clone())
            .collect();
        let qualifier = qualifier.map(|typed| match typed {
            Typed::Value(ty) => Qualifier::Value(ty.clone()),
            Typed::Class(class) => Qualifier::Class(*class),
            Typed::Package(path) => Qualifier::Package(path.clone()),
        });
        if let Some(probe) = &mut self.probe {
            probe.captured = Some(CompletionScope {
                locals,
                receivers,
                qualifier,
            });
        }
    }

    /// A position in the probed text as a position in the file's own text.
    pub(super) fn original_offset(&self, offset: u32) -> u32 {
        match self.probe.as_ref().and_then(|probe| probe.inserted) {
            Some((at, length)) if offset >= at + length => offset - length,
            Some((at, _)) if offset > at => at,
            _ => offset,
        }
    }
}
