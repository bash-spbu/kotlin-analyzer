//! `textDocument/implementation`: from an `expect` declaration, or a use of
//! one, to its `actual`s. *(Not yet: subclasses and overrides.)*

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
    let mut actuals: Vec<ItemLocation> = Vec::new();
    for declaration in declarations {
        if hir::multiplatform_side(db, declaration) != MultiplatformSide::Expect {
            continue;
        }
        for counterpart in hir::expect_actual_family(db, declaration) {
            let is_actual = hir::multiplatform_side(db, counterpart) == MultiplatformSide::Actual;
            if is_actual && !actuals.contains(&counterpart) {
                actuals.push(counterpart);
            }
        }
    }
    actuals
        .into_iter()
        .filter_map(|location| NavigationTarget::of(db, location))
        .collect()
}
