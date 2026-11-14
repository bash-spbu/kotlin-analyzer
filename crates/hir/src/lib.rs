//! The semantic model as features see it: what a name refers to, what a
//! declaration is and where — one API in front of `hir-def` and `hir-ty`, so features never depend on how the layers below are split.

pub use base_db::{
    ArchiveKind, FileChange, FileId, IndexedLibrary, Language, LibrarySource, ProjectModel,
    RootDatabase, SourceDatabase, SourceFile, SourceSetId, parse, salsa,
};
pub use hir_def::def_map::{
    importable_names, package_exists, package_names, package_prefix_exists, packages_declaring,
    sees, visible_top_level,
};
pub use hir_def::expect_actual::{
    Side as MultiplatformSide, expect_actual_family, side_of as multiplatform_side,
};
pub use hir_def::item_tree::{
    ClassKind, Import, ImportTarget, Item, ItemId, ItemKind, ItemRanges, ItemSourceMap, ItemTree,
    Modifiers, Parameter, Signature, TypeArgument, TypeRef, Visibility,
};
pub use hir_def::library::{
    LibrarySourceLocation, index_library, library_declaration_source, release_letter,
};
pub use hir_def::resolver::{
    DEFAULT_IMPORT_PACKAGES, PLATFORM_DEFAULT_IMPORT_PACKAGES, Resolution, default_imports_visible,
    is_classifier, resolve_import_path, resolve_type_path, resolve_type_path_in_declaration,
};
pub use hir_def::{
    ItemContainer, ItemLocation, file_item_source_map, file_item_tree, item_source_map, item_tree,
    source_set_def_map,
};
pub use hir_ty::infer::{
    ArgumentMismatch, Certainty, InferenceResult, Local, LocalId, LocalKind, NameSite,
    NonExhaustiveWhen, Reference, ResolvedName, UnresolvedName, UnsafeCall, declaration_type,
    infer_file,
};
pub use hir_ty::infer::{CompletionScope, PLACEHOLDER, Qualifier, completion_scope};
pub use hir_ty::members::{Member, Types};
pub use hir_ty::overrides::override_family;
pub use hir_ty::{Nullability, Ty, display as display_type};
pub use intern::{Name, Path};

/// The type a type reference is written as, in Kotlin syntax.
pub fn type_ref_text(type_ref: &TypeRef) -> String {
    hir_def::item_tree::type_text(type_ref)
}

/// Whether the top-level `declaration` is an extension a value of type
/// `receiver` can be used with (from `from`'s point of view).
pub fn extends(
    db: &dyn SourceDatabase,
    from: SourceFile,
    declaration: ItemLocation,
    receiver: &Ty,
) -> bool {
    let types = hir_ty::members::Types::new(db, from);
    let tree = item_tree(db, declaration.container);
    let written = match &tree.item(declaration.item).signature {
        Signature::Function {
            receiver: Some(written),
            ..
        }
        | Signature::Property {
            receiver: Some(written),
            ..
        } => written,
        _ => return false,
    };
    let scope = hir_ty::TypeScope::of_declaration(db, from, declaration);
    let declared = hir_ty::lower_type(db, scope, written);
    types.fits(&receiver.clone().non_null(), &declared) != hir_ty::infer::Fit::No
}
