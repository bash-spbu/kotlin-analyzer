//! The semantic model as features see it: what a name refers to, what a
//! declaration is and where — one API in front of `hir-def` (and, later,
//! `hir-ty`), so features never depend on how the layers below are split.

pub use base_db::{
    FileChange, FileId, Language, ProjectModel, RootDatabase, SourceDatabase, SourceFile,
    SourceSetId, parse, salsa,
};
pub use hir_def::item_tree::{
    ClassKind, Import, ImportTarget, Item, ItemIndex, ItemKind, ItemTree, Modifiers, Parameter,
    Signature, TypeArgument, TypeRef, Visibility,
};
pub use hir_def::resolver::{Resolution, resolve_import_path, resolve_type_path};
pub use hir_def::{ItemLocation, file_item_tree, source_set_def_map};
pub use intern::{Name, Path};

/// The type a type reference is written as, in Kotlin syntax.
pub fn type_ref_text(type_ref: &TypeRef) -> String {
    hir_def::item_tree::type_text(type_ref)
}
