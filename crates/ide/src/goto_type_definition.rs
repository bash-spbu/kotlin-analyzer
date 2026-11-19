//! `textDocument/typeDefinition`: from a name to the classes of its type —
//! the class itself, then its type arguments' (`List<Order>`: `List`,
//! `Order`), as rust-analyzer does.

use hir::{FileId, ItemKind, ItemLocation, RootDatabase, SourceDatabase, Ty};

use crate::goto_definition::{NameTarget, name_at};
use crate::navigation_target::NavigationTarget;

pub(crate) fn goto_type_definition(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<NavigationTarget> {
    let Some(name) = name_at(db, file, offset) else {
        return Vec::new();
    };
    let in_body = name.ty.filter(|ty| !ty.is_unknown());
    let ty = in_body
        .or_else(|| match &name.target {
            NameTarget::Local { local, .. } => Some(local.ty.clone()),
            NameTarget::Declarations(declarations) => match declarations[..] {
                [only] => declared_type(db, only),
                _ => None,
            },
        })
        .or_else(|| declared_local_type(db, file, name.range.start));
    let mut classes = Vec::new();
    if let Some(ty) = ty {
        collect_classes(&ty, &mut classes);
    }
    classes
        .into_iter()
        .filter_map(|class| NavigationTarget::of(db, class))
        .collect()
}

/// A declaration's type: a class is its own; a workspace property's or
/// function's, as inferred where it's declared.
fn declared_type(db: &RootDatabase, declaration: ItemLocation) -> Option<Ty> {
    let tree = hir::item_tree(db, declaration.container);
    if matches!(tree.item(declaration.item).kind, ItemKind::Class(_)) {
        return Some(Ty::Class(hir::ClassType {
            class: declaration,
            arguments: Box::new([]),
            nullability: hir::Nullability::NotNull,
        }));
    }
    let file = declaration.file()?;
    Some(hir::declaration_type(db, file, declaration.item)).filter(|ty| !ty.is_unknown())
}

/// The type of the local declared at `start`: a primary constructor's
/// `val` parameter, which is also the class's property.
fn declared_local_type(db: &RootDatabase, file: FileId, start: u32) -> Option<Ty> {
    let source = db.source_file(file)?;
    let inference = hir::infer_file(db, source);
    let local = inference.local_declared_at(start)?;
    Some(inference.local(local).ty.clone()).filter(|ty| !ty.is_unknown())
}

/// The classes `ty` names, its own first, each once.
fn collect_classes(ty: &Ty, classes: &mut Vec<ItemLocation>) {
    match ty {
        Ty::Class(class) => {
            if !classes.contains(&class.class) {
                classes.push(class.class);
            }
            for argument in class.arguments.iter() {
                collect_classes(argument, classes);
            }
        }
        Ty::Function(function) => {
            for part in function
                .receiver
                .iter()
                .chain(function.parameters.iter())
                .chain(std::iter::once(&function.result))
            {
                collect_classes(part, classes);
            }
        }
        Ty::TypeParameter { .. } | Ty::Nothing { .. } | Ty::Unknown => {}
    }
}
