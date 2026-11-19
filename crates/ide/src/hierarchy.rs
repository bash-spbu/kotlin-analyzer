//! Type and call hierarchies: a class's direct supertypes and subtypes; a
//! function's callers (grouped by the declaration the call is in) and
//! callees (grouped by the function called).
//!
//! An item carries what finds it again when the editor asks to expand it:
//! a class by its package and names (a library's too), a workspace
//! function by where its name is.

use hir::{FileId, ItemKind, ItemLocation, Name, Path, RootDatabase, SourceDatabase};
use syntax::TextRange;

use crate::NavigationTarget;
use crate::goto_definition::{NameTarget, name_at};
use crate::references::{self, FileRange};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HierarchyItem {
    pub target: NavigationTarget,
    /// How to find it again; `None` for one that can't be expanded (a
    /// library's function).
    pub key: Option<HierarchyKey>,
}

/// What finds a hierarchy item's declaration again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HierarchyKey {
    /// A class: its package, and its name with the classes it's nested in
    /// (`Map`, `Entry`) — resolved from the file the hierarchy started in.
    Class { package: String, names: Vec<String> },
    /// A workspace declaration: where its name is.
    Declaration { file: FileId, offset: u32 },
}

/// A call into or out of a function: the other function, and where the
/// calls are (in the caller's file).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HierarchyCall {
    pub item: HierarchyItem,
    pub ranges: Vec<FileRange>,
}

pub(crate) fn prepare_type_hierarchy(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<HierarchyItem> {
    declarations_at(db, file, offset)
        .into_iter()
        .filter(|declaration| is_class(db, *declaration))
        .filter_map(|declaration| class_item(db, declaration))
        .collect()
}

pub(crate) fn supertypes(
    db: &RootDatabase,
    from: FileId,
    key: &HierarchyKey,
) -> Vec<HierarchyItem> {
    let (Some(class), Some(source)) = (class_of(db, from, key), db.source_file(from)) else {
        return Vec::new();
    };
    let types = hir::Types::new(db, source);
    types
        .direct_supertypes(&types.self_type(class))
        .into_iter()
        .filter_map(|supertype| class_item(db, supertype.class))
        .collect()
}

pub(crate) fn subtypes(db: &RootDatabase, from: FileId, key: &HierarchyKey) -> Vec<HierarchyItem> {
    let Some(class) = class_of(db, from, key) else {
        return Vec::new();
    };
    hir::direct_subclasses(db, class)
        .into_iter()
        .filter_map(|subclass| class_item(db, subclass))
        .collect()
}

pub(crate) fn prepare_call_hierarchy(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Vec<HierarchyItem> {
    declarations_at(db, file, offset)
        .into_iter()
        .filter(|declaration| is_callable(db, *declaration))
        .filter_map(|declaration| declaration_item(db, declaration))
        .collect()
}

/// Where the function is called, grouped by the declaration each call is
/// in (a function, a property's initializer).
pub(crate) fn incoming_calls(db: &RootDatabase, key: &HierarchyKey) -> Vec<HierarchyCall> {
    let Some(callee) = declaration_of(db, key) else {
        return Vec::new();
    };
    let mut calls: Vec<HierarchyCall> = Vec::new();
    for occurrence in references::declaration_occurrences(db, callee) {
        let Some(caller) = enclosing_declaration(db, occurrence.file, occurrence.range) else {
            continue;
        };
        let Some(item) = declaration_item(db, caller) else {
            continue;
        };
        match calls.iter_mut().find(|call| call.item == item) {
            Some(call) => call.ranges.push(occurrence),
            None => calls.push(HierarchyCall {
                item,
                ranges: vec![occurrence],
            }),
        }
    }
    calls
}

/// The functions the declaration calls, each with where.
pub(crate) fn outgoing_calls(db: &RootDatabase, key: &HierarchyKey) -> Vec<HierarchyCall> {
    let Some(caller) = declaration_of(db, key) else {
        return Vec::new();
    };
    let Some(source) = caller.file() else {
        return Vec::new();
    };
    let file = source.file_id(db);
    let body = hir::file_item_source_map(db, source)
        .ranges(caller.item)
        .range;
    let inference = hir::infer_file(db, source);
    let mut calls: Vec<HierarchyCall> = Vec::new();
    let mut names: Vec<(u32, ItemLocation)> = inference
        .names()
        .filter(|(start, _)| body.contains(*start as usize))
        .filter_map(|(start, resolved)| match resolved.reference {
            hir::Reference::Declaration(location) => Some((start, location)),
            _ => None,
        })
        .filter(|(_, location)| is_callable(db, *location))
        .collect();
    names.sort_by_key(|(start, _)| *start);
    for (start, callee) in names {
        let name = hir::item_tree(db, callee.container).item(callee.item).name;
        let range = FileRange {
            file,
            range: TextRange {
                start,
                end: start + name.as_str().len() as u32,
            },
        };
        let Some(item) = declaration_item(db, callee) else {
            continue;
        };
        match calls.iter_mut().find(|call| call.item == item) {
            Some(call) => call.ranges.push(range),
            None => calls.push(HierarchyCall {
                item,
                ranges: vec![range],
            }),
        }
    }
    calls
}

fn declarations_at(db: &RootDatabase, file: FileId, offset: usize) -> Vec<ItemLocation> {
    match name_at(db, file, offset).map(|name| name.target) {
        Some(NameTarget::Declarations(declarations)) => declarations,
        _ => Vec::new(),
    }
}

fn is_class(db: &RootDatabase, declaration: ItemLocation) -> bool {
    let kind = hir::item_tree(db, declaration.container)
        .item(declaration.item)
        .kind;
    matches!(kind, ItemKind::Class(_))
}

fn is_callable(db: &RootDatabase, declaration: ItemLocation) -> bool {
    let kind = hir::item_tree(db, declaration.container)
        .item(declaration.item)
        .kind;
    matches!(kind, ItemKind::Function | ItemKind::Constructor)
}

/// A class as an item, found again by its package and names.
fn class_item(db: &RootDatabase, class: ItemLocation) -> Option<HierarchyItem> {
    let target = NavigationTarget::of(db, class)?;
    let tree = hir::item_tree(db, class.container);
    let mut names = Vec::new();
    let mut current = Some(class.item);
    while let Some(index) = current {
        let item = tree.item(index);
        names.push(item.name.to_string());
        current = item.parent;
    }
    names.reverse();
    Some(HierarchyItem {
        target,
        key: Some(HierarchyKey::Class {
            package: tree.package.to_string(),
            names,
        }),
    })
}

/// A declaration as an item: a workspace one found again by where its
/// name is; a library's not expanded.
fn declaration_item(db: &RootDatabase, declaration: ItemLocation) -> Option<HierarchyItem> {
    let target = NavigationTarget::of(db, declaration)?;
    let key = declaration.file().map(|source| {
        let name_range = hir::file_item_source_map(db, source)
            .ranges(declaration.item)
            .name_range;
        HierarchyKey::Declaration {
            file: source.file_id(db),
            offset: name_range.start,
        }
    });
    Some(HierarchyItem { target, key })
}

fn class_of(db: &RootDatabase, from: FileId, key: &HierarchyKey) -> Option<ItemLocation> {
    match key {
        HierarchyKey::Class { package, names } => {
            let source = db.source_file(from)?;
            let names: Vec<Name> = names.iter().map(|name| Name::new(name)).collect();
            match hir::resolve_qualified(db, source, &Path::parse(package), &names) {
                hir::Resolution::Declaration(class) => Some(class),
                _ => None,
            }
        }
        HierarchyKey::Declaration { .. } => declaration_of(db, key),
    }
}

fn declaration_of(db: &RootDatabase, key: &HierarchyKey) -> Option<ItemLocation> {
    let HierarchyKey::Declaration { file, offset } = key else {
        return None;
    };
    declarations_at(db, *file, *offset as usize)
        .into_iter()
        .next()
}

/// The innermost function, constructor or property whose declaration
/// holds `range` of `file`: where a call there is made from.
fn enclosing_declaration(
    db: &RootDatabase,
    file: FileId,
    range: TextRange,
) -> Option<ItemLocation> {
    let source = db.source_file(file)?;
    let tree = hir::file_item_tree(db, source);
    let source_map = hir::file_item_source_map(db, source);
    tree.indexed_items()
        .filter(|(_, item)| {
            matches!(
                item.kind,
                ItemKind::Function | ItemKind::Constructor | ItemKind::Property { .. }
            )
        })
        .map(|(index, _)| (index, source_map.ranges(index).range))
        .filter(|(_, declared)| declared.contains_range(range))
        .min_by_key(|(_, declared)| declared.len())
        .map(|(index, _)| ItemLocation::in_file(source, index))
}
