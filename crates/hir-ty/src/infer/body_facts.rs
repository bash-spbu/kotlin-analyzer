//! What checks over a body read once its inference is done (or done as far
//! as they look): the syntax, the names inference resolved and the types —
//! none of the walk's state. The checks after inference (see
//! `validation`) run on these alone; the ones inference needs midway
//! (whether a `when` covers every case, for the flow after it) only look
//! at what's already inferred.

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::kotlin_type_ref;
use syntax::{Parse, SyntaxNode, named_children};

use super::placement::Placement;
use super::{InferenceResult, ResolvedName};
use crate::lower::lower_type;
use crate::members::Types;
use crate::ty::Ty;

pub(super) struct BodyFacts<'facts, 'db> {
    pub(super) db: &'db dyn SourceDatabase,
    pub(super) file: SourceFile,
    pub(super) parse: &'db Parse,
    pub(super) placement: &'facts Placement,
    pub(super) types: &'facts Types<'db>,
    pub(super) result: &'facts InferenceResult,
}

impl<'db> BodyFacts<'_, 'db> {
    pub(super) fn text(&self, node: SyntaxNode<'_>) -> &'db str {
        &self.parse.text()[node.byte_range()]
    }

    /// What inference resolved the name `node` to, if it did.
    pub(super) fn recorded_at(&self, node: SyntaxNode<'_>) -> Option<&ResolvedName> {
        self.result.name_at(node.start_byte() as u32)
    }

    /// The type written as a child of `node` (`x: T`, `is T`, `as T`).
    pub(super) fn type_child<'tree>(&self, node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        named_children(node).find(|child| syntax::ast::kotlin::is_type(*child))
    }

    /// A type as written at `node`: resolved where it's written.
    pub(super) fn written_type(&self, node: SyntaxNode<'_>) -> Ty {
        if node.kind() == "type_identifier" {
            // A bare class name, as `Type::class` and `Type::member` write it.
            let name = intern::Name::new(self.text(node).trim_matches('`'));
            let path = intern::Path::new([name]);
            return match self.placement.resolve_type_path(
                self.db,
                self.file,
                &path,
                node.start_byte(),
            ) {
                hir_def::resolver::Resolution::Declaration(class) => {
                    let mut class_type = self.types.self_type(class);
                    class_type.arguments =
                        class_type.arguments.iter().map(|_| Ty::Unknown).collect();
                    Ty::Class(class_type)
                }
                _ => Ty::Error,
            };
        }
        let type_ref = kotlin_type_ref(self.parse, node);
        lower_type(
            self.db,
            self.placement
                .type_scope(self.db, self.file, node.start_byte()),
            &type_ref,
        )
    }
}
