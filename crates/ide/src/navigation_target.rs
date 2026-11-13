//! A declaration as a place to go to.

use std::path::PathBuf;
use std::sync::Arc;

use hir::{ClassKind, FileId, ItemKind, ItemLocation, Local, RootDatabase, SourceFile};
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
    /// A local variable, parameter or local function.
    Variable,
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

/// Where a navigation target's text lives.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NavigationFile {
    /// A workspace file.
    Workspace(FileId),
    /// An entry of a library's sources archive, with its text (the server
    /// writes it somewhere the editor can open).
    Library {
        archive: PathBuf,
        entry: String,
        text: Arc<str>,
    },
}

/// A declaration to navigate to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NavigationTarget {
    pub file: NavigationFile,
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
    /// `None` for a library declaration whose source isn't available.
    pub fn of(db: &RootDatabase, location: ItemLocation) -> Option<NavigationTarget> {
        let tree = hir::item_tree(db, location.container);
        let item = tree.item(location.item);
        let container = match item.parent {
            Some(parent) => Some(tree.item(parent).name.to_string()),
            None if tree.package.is_root() => None,
            None => Some(tree.package.to_string()),
        };
        let (file, full_range, focus_range) = match location.file() {
            Some(file) => (
                NavigationFile::Workspace(file.file_id(db)),
                item.range,
                item.name_range,
            ),
            None => {
                let source = hir::library_declaration_source(db, location)?;
                (
                    NavigationFile::Library {
                        archive: source.archive,
                        entry: source.entry,
                        text: source.text,
                    },
                    source.full_range,
                    source.name_range,
                )
            }
        };
        Some(NavigationTarget {
            file,
            name: item.name.to_string(),
            kind: SymbolKind::of(item.kind, item.parent.is_some()),
            full_range,
            focus_range,
            container,
        })
    }

    /// A local declared in `file`.
    pub fn of_local(db: &RootDatabase, file: SourceFile, local: &Local) -> NavigationTarget {
        NavigationTarget {
            file: NavigationFile::Workspace(file.file_id(db)),
            name: local.name.to_string(),
            kind: SymbolKind::Variable,
            full_range: local.name_range,
            focus_range: local.name_range,
            container: None,
        }
    }
}
