//! A declaration as a place to go to.

use hir::{ClassKind, FileId, ItemKind, ItemLocation, RootDatabase};
use syntax::TextRange;

/// What a declaration is, as editors show it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolKind {
    Class,
    Interface,
    Enum,
    Object,
    Annotation,
    Function,
    Method,
    Property,
    Field,
    Constructor,
    EnumMember,
    TypeAlias,
}

impl SymbolKind {
    pub fn of(kind: ItemKind, is_member: bool) -> SymbolKind {
        match kind {
            ItemKind::Class(ClassKind::Class) => SymbolKind::Class,
            ItemKind::Class(ClassKind::Interface) => SymbolKind::Interface,
            ItemKind::Class(ClassKind::Enum) => SymbolKind::Enum,
            ItemKind::Class(ClassKind::Annotation) => SymbolKind::Annotation,
            ItemKind::Class(ClassKind::Object | ClassKind::CompanionObject) => SymbolKind::Object,
            ItemKind::Function if is_member => SymbolKind::Method,
            ItemKind::Function => SymbolKind::Function,
            ItemKind::Property { .. } if is_member => SymbolKind::Field,
            ItemKind::Property { .. } => SymbolKind::Property,
            ItemKind::TypeAlias => SymbolKind::TypeAlias,
            ItemKind::EnumEntry => SymbolKind::EnumMember,
            ItemKind::Constructor => SymbolKind::Constructor,
        }
    }
}

/// A declaration to navigate to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NavigationTarget {
    pub file: FileId,
    pub name: String,
    pub kind: SymbolKind,
    /// The whole declaration.
    pub full_range: TextRange,
    /// Its name — where the cursor lands.
    pub focus_range: TextRange,
    /// Where it lives: the enclosing class, or the package.
    pub container: Option<String>,
}

impl NavigationTarget {
    pub fn of(db: &RootDatabase, location: ItemLocation) -> NavigationTarget {
        let tree = hir::file_item_tree(db, location.file);
        let item = tree.item(location.item);
        let container = match item.parent {
            Some(parent) => Some(tree.item(parent).name.to_string()),
            None if tree.package.is_root() => None,
            None => Some(tree.package.to_string()),
        };
        NavigationTarget {
            file: location.file.file_id(db),
            name: item.name.to_string(),
            kind: SymbolKind::of(item.kind, item.parent.is_some()),
            full_range: item.range,
            focus_range: item.name_range,
            container,
        }
    }
}
