//! `textDocument/implementation`: from a class or interface, or a use of
//! one, to its subclasses in the workspace; from a function or property to
//! its overrides there; from an `expect` declaration to its `actual`s.

use hir::{FileId, ItemLocation, MultiplatformSide, RootDatabase};

use crate::goto_definition::{NameTarget, name_at};
use crate::navigation_target::NavigationTarget;

pub(crate) fn goto_implementation(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<NavigationTarget> {
    let Some(name) = name_at(db, file, offset) else {
        return Vec::new();
    };
    let NameTarget::Declarations(declarations) = name.target else {
        return Vec::new();
    };
    let mut implementations: Vec<ItemLocation> = Vec::new();
    for declaration in declarations {
        let found = match hir::multiplatform_side(db, declaration) {
            MultiplatformSide::Expect => actuals_of(db, declaration),
            _ => hir::implementations(db, declaration),
        };
        for implementation in found {
            if !implementations.contains(&implementation) {
                implementations.push(implementation);
            }
        }
    }
    implementations
        .into_iter()
        .filter_map(|location| NavigationTarget::of(db, location))
        .collect()
}

/// The `actual`s of an `expect` declaration.
fn actuals_of(db: &RootDatabase, declaration: ItemLocation) -> Vec<ItemLocation> {
    hir::expect_actual_family(db, declaration)
        .into_iter()
        .filter(|counterpart| {
            hir::multiplatform_side(db, *counterpart) == MultiplatformSide::Actual
        })
        .collect()
}
