//! Libraries: indexing an archive once, and reading a package's
//! declarations into an [`ItemTree`] when a query needs them.

use std::sync::Arc;

use base_db::{
    ArchiveKind, IndexedLibrary, IndexedPackage, LibraryPackage, LibrarySource, SourceDatabase,
};
use classfile::{ClassContents, LibraryClass, LibraryType, MemberKind, PackageMembers};
use intern::{Name, Path};
use rustc_hash::FxHashMap;
use syntax::TextRange;

use crate::item_tree::{
    ClassKind, Item, ItemIndex, ItemKind, ItemTree, Modifiers, Parameter, Signature, TypeArgument,
    TypeParameter, TypeRef, TypeSegment, Variance, Visibility,
};

/// Where an archive entry's declarations belong.
struct EntryPlace {
    package: Path,
    module: Option<Name>,
}

/// The package (and JDK module) of an archive entry that holds
/// declarations; `None` for anything else.
fn place_of(kind: &ArchiveKind, name: &str) -> Option<EntryPlace> {
    let is_declarations =
        name.ends_with(".class") || name.ends_with(".sig") || name.ends_with(".kotlin_builtins");
    if !is_declarations
        || name.starts_with("META-INF/")
        || name.ends_with("module-info.class")
        || name.ends_with("module-info.sig")
        || name.ends_with("package-info.class")
        || name.ends_with("package-info.sig")
    {
        return None;
    }
    match kind {
        ArchiveKind::Jar => {
            let directory = name
                .rsplit_once('/')
                .map(|(directory, _)| directory)
                .unwrap_or("");
            Some(EntryPlace {
                package: Path::parse(&directory.replace('/', ".")),
                module: None,
            })
        }
        ArchiveKind::JdkSymbols { release } => {
            // `<releases>/<module>/<package path>/<Class>.sig`
            let mut parts = name.splitn(3, '/');
            let releases = parts.next()?;
            let module = parts.next()?;
            let rest = parts.next()?;
            if !releases.contains(*release) {
                return None;
            }
            let directory = rest
                .rsplit_once('/')
                .map(|(directory, _)| directory)
                .unwrap_or("");
            Some(EntryPlace {
                package: Path::parse(&directory.replace('/', ".")),
                module: Some(Name::new(module)),
            })
        }
    }
}

/// Reads every declaration entry of an archive once, keeping per package
/// only the top-level names and where its entries are.
pub fn index_library(source: &LibrarySource) -> Result<IndexedLibrary, String> {
    let mut archive =
        classfile::Archive::open(&source.archive).map_err(|error| error.to_string())?;
    let mut packages: FxHashMap<Path, IndexedPackage> = FxHashMap::default();
    for index in 0..archive.len() {
        let Some(name) = archive.name(index).map(str::to_owned) else {
            continue;
        };
        let Some(place) = place_of(&source.kind, &name) else {
            continue;
        };
        let Some(bytes) = archive.read(index) else {
            continue;
        };
        let package = packages
            .entry(place.package.clone())
            .or_insert_with(|| IndexedPackage {
                package: place.package.clone(),
                names: Vec::new(),
                entries: Vec::new(),
                module: place.module,
            });
        let mut declares_something = false;
        for contents in classfile::read_entry(&name, &bytes) {
            match contents {
                ClassContents::Class(class) => {
                    declares_something = true;
                    if class.name.classes.len() == 1 {
                        package.names.push(class.name.simple_name());
                    }
                }
                ClassContents::PackageMembers(members) => {
                    declares_something = true;
                    package
                        .names
                        .extend(members.members.iter().map(|member| member.name));
                    package
                        .names
                        .extend(members.type_aliases.iter().map(|alias| alias.name));
                }
                ClassContents::Nothing => {}
            }
        }
        if declares_something {
            package.entries.push(index as u32);
        }
    }
    let mut packages: Vec<IndexedPackage> = packages
        .into_values()
        .filter(|package| !package.entries.is_empty())
        .collect();
    for package in &mut packages {
        package.names.sort();
        package.names.dedup();
    }
    Ok(IndexedLibrary { packages })
}

/// A library package's declarations.
#[salsa::tracked(returns(clone), lru = 128)]
pub fn library_package_tree(db: &dyn SourceDatabase, package: LibraryPackage) -> Arc<ItemTree> {
    let library = package.library(db);
    let source = library.source(db).clone();
    let Ok(mut archive) = classfile::Archive::open(&source.archive) else {
        return Arc::new(ItemTree::default());
    };
    let mut classes: Vec<LibraryClass> = Vec::new();
    let mut builtins: Vec<LibraryClass> = Vec::new();
    let mut members: Vec<PackageMembers> = Vec::new();
    for index in package.entries(db).iter() {
        let index = *index as usize;
        let Some(name) = archive.name(index).map(str::to_owned) else {
            continue;
        };
        let Some(bytes) = archive.read(index) else {
            continue;
        };
        let is_builtins = name.ends_with(".kotlin_builtins");
        for contents in classfile::read_entry(&name, &bytes) {
            match contents {
                ClassContents::Class(class) if is_builtins => builtins.push(class),
                ClassContents::Class(class) => classes.push(class),
                ClassContents::PackageMembers(part) => members.push(part),
                ClassContents::Nothing => {}
            }
        }
    }
    // A class with both a class file and a built-ins entry is one class:
    // the class file wins.
    for class in builtins {
        if !classes.iter().any(|existing| existing.name == class.name) {
            classes.push(class);
        }
    }
    Arc::new(lower(package.package(db).clone(), classes, members))
}

/// The classes and top-level members of one package as an item tree.
fn lower(package: Path, mut classes: Vec<LibraryClass>, members: Vec<PackageMembers>) -> ItemTree {
    let mut builder = Builder::default();
    // Outer classes first, so each nested class finds its parent.
    classes.sort_by_key(|class| class.name.classes.len());
    let mut by_nesting: FxHashMap<Box<[Name]>, ItemIndex> = FxHashMap::default();
    let mut source_files = Vec::new();
    for class in classes {
        let parent = match class.name.classes.split_last() {
            Some((_, outer)) if !outer.is_empty() => match by_nesting.get(outer) {
                Some(parent) => Some(*parent),
                // The outer class isn't visible: neither is this one.
                None => continue,
            },
            _ => None,
        };
        let source_file = class.source_file.clone();
        let nesting = class.name.classes.clone();
        let index = builder.class(class, parent);
        if parent.is_none()
            && let Some(source_file) = source_file
        {
            source_files.push((index, source_file.into_boxed_str()));
        }
        by_nesting.insert(nesting, index);
    }
    for part in members {
        for member in part.members {
            let index = builder.member(member, None, false);
            if let Some(source_file) = &part.source_file {
                source_files.push((index, source_file.clone().into_boxed_str()));
            }
        }
        for alias in part.type_aliases {
            let index = builder.push(
                Item {
                    name: alias.name,
                    kind: ItemKind::TypeAlias,
                    name_range: TextRange::default(),
                    range: TextRange::default(),
                    parent: None,
                    children: Box::new([]),
                    visibility: Visibility::Public,
                    modifiers: Modifiers::default(),
                    type_parameters: type_parameters(&alias.type_parameters),
                    signature: Signature::TypeAlias {
                        target: type_ref(&alias.expanded),
                    },
                },
                None,
            );
            if let Some(source_file) = &part.source_file {
                source_files.push((index, source_file.clone().into_boxed_str()));
            }
        }
    }
    let (items, top_level) = builder.finish();
    ItemTree {
        package,
        source_files: source_files.into_boxed_slice(),
        imports: Box::new([]),
        top_level: top_level.into_boxed_slice(),
        items: items.into_boxed_slice(),
    }
}

#[derive(Default)]
struct Builder {
    items: Vec<Item>,
    top_level: Vec<ItemIndex>,
    children: FxHashMap<ItemIndex, Vec<ItemIndex>>,
}

impl Builder {
    fn push(&mut self, item: Item, parent: Option<ItemIndex>) -> ItemIndex {
        let index = ItemIndex(self.items.len() as u32);
        self.items.push(Item { parent, ..item });
        match parent {
            Some(parent) => self.children.entry(parent).or_default().push(index),
            None => self.top_level.push(index),
        }
        index
    }

    /// The items, each given its children.
    fn finish(mut self) -> (Vec<Item>, Vec<ItemIndex>) {
        for (parent, children) in self.children {
            self.items[parent.0 as usize].children = children.into_boxed_slice();
        }
        (self.items, self.top_level)
    }

    fn class(&mut self, class: LibraryClass, parent: Option<ItemIndex>) -> ItemIndex {
        let kind = match class.kind {
            classfile::ClassKind::Class => ClassKind::Class,
            classfile::ClassKind::Interface => ClassKind::Interface,
            classfile::ClassKind::Enum => ClassKind::Enum,
            classfile::ClassKind::Annotation => ClassKind::Annotation,
            classfile::ClassKind::Object => ClassKind::Object,
            classfile::ClassKind::CompanionObject => ClassKind::CompanionObject,
        };
        let primary_constructor = None;
        let index = self.push(
            Item {
                name: class.name.simple_name(),
                kind: ItemKind::Class(kind),
                name_range: TextRange::default(),
                range: TextRange::default(),
                parent,
                children: Box::new([]),
                visibility: visibility(class.visibility),
                modifiers: modifiers(&class.modifiers),
                type_parameters: type_parameters(&class.type_parameters),
                signature: Signature::Class {
                    supertypes: class.supertypes.iter().map(type_ref).collect(),
                    primary_constructor,
                },
            },
            parent,
        );
        let is_java = class.origin == classfile::Origin::Java;
        for entry in class.enum_entries {
            self.push(
                Item {
                    name: entry,
                    kind: ItemKind::EnumEntry,
                    name_range: TextRange::default(),
                    range: TextRange::default(),
                    parent: Some(index),
                    children: Box::new([]),
                    visibility: Visibility::Public,
                    modifiers: Modifiers::default(),
                    type_parameters: Box::new([]),
                    signature: Signature::None,
                },
                Some(index),
            );
        }
        for member in class.members {
            self.member(member, Some(index), is_java);
        }
        index
    }

    fn member(
        &mut self,
        member: classfile::Member,
        parent: Option<ItemIndex>,
        is_java: bool,
    ) -> ItemIndex {
        let mut kept = modifiers(&member.modifiers);
        if is_java && member.modifiers.is_static {
            kept.insert(Modifiers::STATIC);
        }
        let (kind, signature) = match member.kind {
            MemberKind::Function => (
                ItemKind::Function,
                Signature::Function {
                    receiver: member.receiver.as_ref().map(type_ref),
                    parameters: parameters(&member.parameters),
                    return_type: member.return_type.as_ref().map(type_ref),
                },
            ),
            MemberKind::Constructor => (
                ItemKind::Constructor,
                Signature::Function {
                    receiver: None,
                    parameters: parameters(&member.parameters),
                    return_type: None,
                },
            ),
            MemberKind::Property { mutable } => (
                ItemKind::Property { mutable },
                Signature::Property {
                    receiver: member.receiver.as_ref().map(type_ref),
                    type_ref: member.return_type.as_ref().map(type_ref),
                },
            ),
        };
        self.push(
            Item {
                name: member.name,
                kind,
                name_range: TextRange::default(),
                range: TextRange::default(),
                parent,
                children: Box::new([]),
                visibility: visibility(member.visibility),
                modifiers: kept,
                type_parameters: type_parameters(&member.type_parameters),
                signature,
            },
            parent,
        )
    }
}

fn visibility(visibility: classfile::Visibility) -> Visibility {
    match visibility {
        classfile::Visibility::Public => Visibility::Public,
        classfile::Visibility::Protected => Visibility::Protected,
    }
}

fn modifiers(modifiers: &classfile::Modifiers) -> Modifiers {
    let mut kept = Modifiers::default();
    let flags = [
        (modifiers.is_abstract, Modifiers::ABSTRACT),
        (modifiers.is_open, Modifiers::OPEN),
        (modifiers.is_sealed, Modifiers::SEALED),
        (modifiers.is_data, Modifiers::DATA),
        (modifiers.is_inner, Modifiers::INNER),
        (modifiers.is_value, Modifiers::VALUE),
        (modifiers.is_fun_interface, Modifiers::FUN_INTERFACE),
        (modifiers.is_expect, Modifiers::EXPECT),
        (modifiers.is_static, Modifiers::STATIC),
        (modifiers.is_suspend, Modifiers::SUSPEND),
        (modifiers.is_inline, Modifiers::INLINE),
        (modifiers.is_operator, Modifiers::OPERATOR),
        (modifiers.is_infix, Modifiers::INFIX),
        (modifiers.is_const, Modifiers::CONST),
        (modifiers.is_deprecated, Modifiers::DEPRECATED),
    ];
    for (present, modifier) in flags {
        if present {
            kept.insert(modifier);
        }
    }
    kept
}

fn type_parameters(parameters: &[classfile::TypeParameter]) -> Box<[TypeParameter]> {
    parameters
        .iter()
        .map(|parameter| TypeParameter {
            name: parameter.name,
            bounds: parameter.bounds.iter().map(type_ref).collect(),
        })
        .collect()
}

fn parameters(parameters: &[classfile::ValueParameter]) -> Box<[Parameter]> {
    parameters
        .iter()
        .map(|parameter| Parameter {
            name: parameter.name,
            type_ref: Some(type_ref(&parameter.type_ref)),
            has_default: parameter.has_default,
            vararg: parameter.vararg,
        })
        .collect()
}

pub(crate) fn type_ref(library_type: &LibraryType) -> TypeRef {
    match library_type {
        LibraryType::Class {
            name,
            arguments,
            nullable,
        } => {
            let last = name.classes.len().saturating_sub(1);
            let classes = name
                .classes
                .iter()
                .enumerate()
                .map(|(position, class)| TypeSegment {
                    name: *class,
                    arguments: if position == last {
                        arguments.iter().map(type_argument).collect()
                    } else {
                        Box::new([])
                    },
                })
                .collect();
            TypeRef::Qualified {
                package: name.package.clone(),
                classes,
                nullable: *nullable,
            }
        }
        LibraryType::TypeParameter { name, nullable } => TypeRef::TypeParameter {
            name: *name,
            nullable: *nullable,
        },
        LibraryType::Function {
            receiver,
            parameters,
            result,
            suspend,
            nullable,
        } => TypeRef::Function {
            receiver: receiver
                .as_ref()
                .map(|receiver| Box::new(type_ref(receiver))),
            parameters: parameters.iter().map(type_ref).collect(),
            result: Box::new(type_ref(result)),
            suspend: *suspend,
            nullable: *nullable,
        },
        LibraryType::Primitive(name) => TypeRef::Primitive { name: *name },
        LibraryType::Array(element) => TypeRef::Array {
            element: Box::new(type_ref(element)),
        },
        LibraryType::Unreadable => TypeRef::Unreadable,
    }
}

fn type_argument(argument: &classfile::TypeArgument) -> TypeArgument {
    match argument {
        classfile::TypeArgument::Star => TypeArgument::Star,
        classfile::TypeArgument::Type {
            variance,
            type_ref: inner,
        } => TypeArgument::Type {
            variance: match variance {
                classfile::Variance::Invariant => Variance::Invariant,
                classfile::Variance::In => Variance::In,
                classfile::Variance::Out => Variance::Out,
            },
            type_ref: type_ref(inner),
        },
    }
}

/// The release letter `ct.sym` uses for a Java version: `8`, `9`, then
/// `A` for 10 onwards.
pub fn release_letter(version: u32) -> Option<char> {
    match version {
        8 | 9 => char::from_digit(version, 10),
        10..=35 => char::from_u32('A' as u32 + (version - 10)),
        _ => None,
    }
}

/// A library declaration found in its library's sources archive.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LibrarySourceLocation {
    pub archive: std::path::PathBuf,
    /// The entry's path inside the archive (`java.base/java/util/List.java`).
    pub entry: String,
    pub text: Arc<str>,
    pub name_range: TextRange,
    pub full_range: TextRange,
}

/// Where a library declaration is written in its sources archive, by the
/// file it was compiled from (or, without that, by searching its package's
/// directory), then by its chain of names.
pub fn library_declaration_source(
    db: &dyn SourceDatabase,
    location: crate::ItemLocation,
) -> Option<LibrarySourceLocation> {
    let crate::ItemContainer::Library(package) = location.container else {
        return None;
    };
    let tree = library_package_tree(db, package);
    let library = package.library(db);
    let source = library.source(db).clone();
    let sources = source.sources.as_ref()?;
    let chain = name_chain(&tree, location.item);
    let top_level = *chain.first()?;
    let mut directory = package
        .package(db)
        .segments()
        .iter()
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>()
        .join("/");
    if let Some(module) = package.module(db) {
        directory = if directory.is_empty() {
            module.to_string()
        } else {
            format!("{module}/{directory}")
        };
    }
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    let mut archive = classfile::Archive::open(sources).ok()?;
    let recorded = tree
        .source_files
        .iter()
        .find(|(index, _)| *index == top_level)
        .map(|(_, file)| format!("{prefix}{file}"));
    let outermost = tree.item(top_level).name;
    let mut candidates: Vec<String> = recorded.into_iter().collect();
    for extension in ["java", "kt"] {
        candidates.push(format!("{prefix}{outermost}.{extension}"));
    }
    // Then every source file of the package directory.
    for index in 0..archive.len() {
        let Some(name) = archive.name(index) else {
            continue;
        };
        let in_directory = name.strip_prefix(&prefix).is_some_and(|rest| {
            !rest.contains('/') && (rest.ends_with(".kt") || rest.ends_with(".java"))
        });
        if in_directory && !candidates.iter().any(|candidate| candidate == name) {
            candidates.push(name.to_owned());
        }
    }
    let names: Vec<(Name, bool)> = chain
        .iter()
        .map(|index| {
            (
                tree.item(*index).name,
                tree.item(*index).kind.is_classifier(),
            )
        })
        .collect();
    for entry in candidates {
        let Some(index) = archive.index_of(&entry) else {
            continue;
        };
        let Some(bytes) = archive.read(index) else {
            continue;
        };
        let text: Arc<str> = Arc::from(String::from_utf8_lossy(&bytes).as_ref());
        let Some(language) = syntax::Language::of_path(std::path::Path::new(&entry)) else {
            continue;
        };
        let parse = syntax::parse(text.clone(), language);
        let source_tree = match language {
            syntax::Language::Kotlin => crate::item_tree::lower_kotlin(&parse),
            syntax::Language::Java => crate::item_tree::lower_java(&parse),
        };
        if let Some(found) = find_by_names(&source_tree, &names) {
            let item = source_tree.item(found);
            return Some(LibrarySourceLocation {
                archive: sources.clone(),
                entry,
                name_range: item.name_range,
                full_range: item.range,
                text,
            });
        }
    }
    None
}

/// The items from the top-level one down to `item`.
fn name_chain(tree: &ItemTree, item: ItemIndex) -> Vec<ItemIndex> {
    let mut chain = vec![item];
    let mut current = item;
    while let Some(parent) = tree.item(current).parent {
        chain.push(parent);
        current = parent;
    }
    chain.reverse();
    chain
}

/// The item reached by following `names` (each: a name, and whether it
/// is a classifier) from the top level.
fn find_by_names(tree: &ItemTree, names: &[(Name, bool)]) -> Option<ItemIndex> {
    let mut level: &[ItemIndex] = &tree.top_level;
    let mut found = None;
    for (name, is_classifier) in names {
        let next = level.iter().copied().find(|index| {
            let item = tree.item(*index);
            item.name == *name && item.kind.is_classifier() == *is_classifier
        })?;
        found = Some(next);
        level = &tree.item(next).children;
    }
    found
}
