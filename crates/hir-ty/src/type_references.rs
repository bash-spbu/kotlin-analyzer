//! The classes a file's written types name, resolved once per file: every
//! segment of every type (`Outer` and `Inner` of `Outer.Inner`) that
//! resolves to a declaration. The deprecation and opt-in checks both read
//! them.

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::ItemLocation;
use hir_def::resolver::{Resolution, resolve_type_path};
use intern::{Name, Path};
use rustc_hash::FxHashSet;
use syntax::{Language, SyntaxNode, TextRange, named_children};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeReference {
    /// The segment.
    pub range: TextRange,
    pub declaration: ItemLocation,
    /// The class of an annotation (`@Beta`, `@OptIn(…)`): the annotation
    /// names it rather than uses it.
    pub annotation_class: bool,
}

/// Every segment of `file`'s written types that names a declaration, in
/// text order.
#[salsa::tracked(returns(clone), lru = 64)]
pub fn type_references(db: &dyn SourceDatabase, file: SourceFile) -> Arc<[TypeReference]> {
    if file.language(db) != Language::Kotlin {
        return Arc::new([]);
    }
    let parse = base_db::parse(db, file);
    let annotation_classes = annotation_class_types(parse.root());
    let mut found = Vec::new();
    let user_types = syntax::descendants(parse.root()).filter(|node| node.kind() == "user_type");
    for user_type in user_types {
        let annotation_class = annotation_classes.contains(&user_type.start_byte());
        let mut path: Vec<Name> = Vec::new();
        for segment in named_children(user_type).filter(|child| child.kind() == "type_identifier") {
            path.push(Name::new(parse.text_of(segment).trim_matches('`')));
            let resolution =
                resolve_type_path(db, file, &Path::new(path.clone()), segment.start_byte());
            if let Resolution::Declaration(declaration) = resolution {
                found.push(TypeReference {
                    range: TextRange::of(segment),
                    declaration,
                    annotation_class,
                });
            }
        }
    }
    found.into()
}

/// Where the types naming annotation classes start: `Beta` of `@Beta`, of
/// `@Beta(…)`, of `@file:Beta`. Found from the annotations down, without
/// asking each type for its parent (which walks the tree from its root).
fn annotation_class_types(root: SyntaxNode<'_>) -> FxHashSet<usize> {
    let mut found = FxHashSet::default();
    let annotations = syntax::descendants(root)
        .filter(|node| matches!(node.kind(), "annotation" | "file_annotation"));
    for annotation in annotations {
        for child in named_children(annotation) {
            match child.kind() {
                "user_type" => {
                    found.insert(child.start_byte());
                }
                "constructor_invocation" => {
                    if let Some(user_type) =
                        named_children(child).find(|part| part.kind() == "user_type")
                    {
                        found.insert(user_type.start_byte());
                    }
                }
                _ => {}
            }
        }
    }
    found
}
