//! Libraries: indexing an archive once, and reading a package's
//! declarations into an [`ItemTree`] when a query needs them.

use std::sync::Arc;

use base_db::{
    ArchiveKind, ArchiveStamp, IndexedLibrary, IndexedPackage, LibraryPackage, LibrarySource,
    SourceDatabase,
};
use classfile::{ClassContents, LibraryClass, LibraryType, MemberKind, PackageMembers};
use intern::{Name, Path};
use rustc_hash::FxHashMap;
use syntax::TextRange;

use crate::item_tree::{
    ClassKind, Item, ItemId, ItemKind, ItemRanges, ItemTree, ItemTreeBuilder, Modifiers, Parameter,
    Signature, TypeArgument, TypeParameter, TypeRef, TypeSegment, Variance, Visibility,
};

/// Where an archive entry's declarations belong (their package is the one
/// they declare).
struct EntryPlace {
    /// The JDK module, for `ct.sym` entries.
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
        ArchiveKind::Jar => Some(EntryPlace { module: None }),
        ArchiveKind::JdkSymbols { release, .. } => {
            // `<releases>/<module>/<package path>/<Class>.sig`
            let mut parts = name.splitn(3, '/');
            let releases = parts.next()?;
            let module = parts.next()?;
            if !releases.contains(*release) {
                return None;
            }
            Some(EntryPlace {
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
    let kind = match source.kind {
        ArchiveKind::JdkSymbols { release, known } => {
            let listed = (0..archive.len()).filter_map(|index| {
                let name = archive.name(index)?;
                name.split('/').next().map(str::to_owned)
            });
            let releases: Vec<String> = listed.collect();
            ArchiveKind::JdkSymbols {
                release: newest_release_at_most(releases.iter().map(String::as_str), release)
                    .unwrap_or(release),
                known,
            }
        }
        ArchiveKind::Jar => ArchiveKind::Jar,
    };
    let mut packages: FxHashMap<Path, IndexedPackage> = FxHashMap::default();
    for index in 0..archive.len() {
        let Some(name) = archive.name(index).map(str::to_owned) else {
            continue;
        };
        let Some(place) = place_of(&kind, &name) else {
            continue;
        };
        let Some(bytes) = archive.read(index) else {
            continue;
        };
        // An entry's declarations go to the package they declare, which
        // for a Kotlin file facade may differ from the entry's directory.
        for contents in classfile::read_entry(&name, &bytes) {
            let (declared_package, names): (Path, Vec<Name>) = match contents {
                ClassContents::Class(class) => {
                    let names = if class.name.classes.len() == 1 {
                        vec![class.name.simple_name()]
                    } else {
                        Vec::new()
                    };
                    (class.name.package.clone(), names)
                }
                ClassContents::PackageMembers(members) => {
                    let names = members
                        .members
                        .iter()
                        .map(|member| member.name)
                        .chain(members.type_aliases.iter().map(|alias| alias.name))
                        .collect();
                    (members.package.clone(), names)
                }
                ClassContents::Nothing => continue,
            };
            let package =
                packages
                    .entry(declared_package.clone())
                    .or_insert_with(|| IndexedPackage {
                        package: declared_package,
                        names: Vec::new(),
                        entries: Vec::new(),
                        module: place.module,
                    });
            package.names.extend(names);
            if package.entries.last() != Some(&(index as u32)) {
                package.entries.push(index as u32);
            }
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
    packages.sort_by_cached_key(|package| package.package.to_string());
    Ok(IndexedLibrary {
        packages,
        stamp: ArchiveStamp::of(&source.archive),
    })
}

/// A library package's declarations.
///
/// Kept for the session, not evicted: every lookup of a library name asks
/// for one, from every analysis thread, and an LRU's bookkeeping is one
/// lock they all meet at (half the time of a workspace pass went to it).
/// What it costs: the trees of the packages the workspace uses stay in
/// memory (about 40 MB on a 600-library workspace).
#[salsa::tracked(returns(clone))]
pub fn library_package_tree(db: &dyn SourceDatabase, package: LibraryPackage) -> Arc<ItemTree> {
    let library = package.library(db);
    // Ties the tree to the archive as indexed: one rewritten in place gets
    // a new stamp on reload, and is read again.
    library.stamp(db);
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
        let own_package = package.package(db);
        for contents in classfile::read_entry(&name, &bytes) {
            match contents {
                ClassContents::Class(class) if class.name.package != *own_package => {}
                ClassContents::Class(class) if is_builtins => builtins.push(class),
                ClassContents::Class(class) => classes.push(class),
                ClassContents::PackageMembers(part) if part.package != *own_package => {}
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
    let mut by_nesting: FxHashMap<Box<[Name]>, ItemId> = FxHashMap::default();
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
    builder.finish(package, source_files)
}

#[derive(Default)]
struct Builder {
    tree: ItemTreeBuilder,
    top_level: Vec<ItemId>,
    children: FxHashMap<ItemId, Vec<ItemId>>,
}

impl Builder {
    fn push(&mut self, item: Item, parent: Option<ItemId>) -> ItemId {
        // Compiled code has no text to point into: no ranges.
        let id = self
            .tree
            .push(Item { parent, ..item }, ItemRanges::default());
        match parent {
            Some(parent) => self.children.entry(parent).or_default().push(id),
            None => self.top_level.push(id),
        }
        id
    }

    /// The tree, each item given its children.
    fn finish(mut self, package: Path, source_files: Vec<(ItemId, Box<str>)>) -> ItemTree {
        for (parent, children) in self.children {
            self.tree.set_children(parent, children);
        }
        let (tree, _) = self
            .tree
            .finish(package, Vec::new(), self.top_level, source_files);
        tree
    }

    fn class(&mut self, class: LibraryClass, parent: Option<ItemId>) -> ItemId {
        let kind = match class.kind {
            classfile::ClassKind::Class => ClassKind::Class,
            classfile::ClassKind::Interface => ClassKind::Interface,
            classfile::ClassKind::Enum => ClassKind::Enum,
            classfile::ClassKind::Annotation => ClassKind::Annotation,
            classfile::ClassKind::Object => ClassKind::Object,
            classfile::ClassKind::CompanionObject => ClassKind::CompanionObject,
        };
        let primary_constructor = None;
        let is_java = class.origin == classfile::Origin::Java;
        let mut class_modifiers = modifiers(&class.modifiers);
        if is_java {
            class_modifiers.insert(Modifiers::PLATFORM);
        }
        let deprecation = class.deprecation.clone();
        let annotations = annotation_paths(&class.annotations);
        let opt_in_marker = class.opt_in_marker.clone();
        let index = self.push(
            Item {
                name: class.name.simple_name(),
                kind: ItemKind::Class(kind),
                parent,
                children: Box::new([]),
                visibility: visibility(class.visibility),
                modifiers: class_modifiers,
                type_parameters: type_parameters(&class.type_parameters),
                signature: Signature::Class {
                    supertypes: class.supertypes.iter().map(type_ref).collect(),
                    primary_constructor,
                },
            },
            parent,
        );
        for entry in class.enum_entries {
            self.push(
                Item {
                    name: entry,
                    kind: ItemKind::EnumEntry,
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
        if let Some(deprecation) = deprecation {
            self.tree.deprecate(index, *deprecation);
        }
        self.tree.annotate(index, annotations);
        if let Some(marker) = opt_in_marker {
            self.tree.mark_opt_in(index, marker);
        }
        index
    }

    fn member(
        &mut self,
        member: classfile::Member,
        parent: Option<ItemId>,
        is_java: bool,
    ) -> ItemId {
        let mut kept = modifiers(&member.modifiers);
        let deprecation = member.deprecation.clone();
        let annotations = annotation_paths(&member.annotations);
        let contract = member
            .contract
            .as_deref()
            .map(|contract| contract.map(&type_ref));
        if is_java && member.modifiers.is_static {
            kept.insert(Modifiers::STATIC);
        }
        if is_java {
            kept.insert(Modifiers::PLATFORM);
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
        let id = self.push(
            Item {
                name: member.name,
                kind,
                parent,
                children: Box::new([]),
                visibility: visibility(member.visibility),
                modifiers: kept,
                type_parameters: type_parameters(&member.type_parameters),
                signature,
            },
            parent,
        );
        if let Some(deprecation) = deprecation {
            self.tree.deprecate(id, *deprecation);
        }
        self.tree.annotate(id, annotations);
        if let Some(contract) = contract {
            self.tree.add_contract(id, contract);
        }
        id
    }
}

/// Annotation classes as qualified paths
/// (`kotlinx.coroutines.ExperimentalCoroutinesApi`).
fn annotation_paths(classes: &[classfile::ClassName]) -> Box<[Path]> {
    classes
        .iter()
        .map(|class| {
            let mut segments: Vec<Name> = class.package.segments().to_vec();
            segments.extend(class.classes.iter().copied());
            Path::new(segments)
        })
        .collect()
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
        (modifiers.is_synthesized, Modifiers::SYNTHESIZED),
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
            reified: false,
            variance: match parameter.variance {
                classfile::Variance::Invariant => Variance::Invariant,
                classfile::Variance::In => Variance::In,
                classfile::Variance::Out => Variance::Out,
            },
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

/// The release `ct.sym` has for `requested`: itself, else the newest one
/// before it — among the releases ct.sym describes in full. A JDK's own
/// release is barely in its `ct.sym` (JDK 21's has two entries for 21: the
/// current API is in its runtime image), and the one before is nearly the
/// same API. `entry_directories` holds each entry's top-level directory:
/// the letters of the releases sharing it (`9A`, `BCDEFGHIJK`).
pub fn newest_release_at_most<'name>(
    entry_directories: impl Iterator<Item = &'name str>,
    requested: char,
) -> Option<char> {
    let mut entries_per_release: FxHashMap<char, usize> = FxHashMap::default();
    let release_directories = entry_directories.filter(|directory| {
        !directory.is_empty()
            && directory
                .chars()
                .all(|char| char.is_ascii_digit() || char.is_ascii_uppercase())
    });
    for directory in release_directories {
        for release in directory.chars() {
            *entries_per_release.entry(release).or_default() += 1;
        }
    }
    let most = entries_per_release.values().copied().max()?;
    entries_per_release
        .into_iter()
        .filter(|(release, entries)| *release <= requested && entries * 2 >= most)
        .map(|(release, _)| release)
        .max()
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

/// A library declaration's text: found in its library's sources archive,
/// or rendered from its class files.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LibrarySourceLocation {
    /// The sources archive — or, for a rendered file, the compiled one.
    pub archive: std::path::PathBuf,
    /// The entry's path inside the archive (`java.base/java/util/List.java`);
    /// a rendered file's under `decompiled/`.
    pub entry: String,
    pub text: Arc<str>,
    pub name_range: TextRange,
    pub full_range: TextRange,
    pub origin: SourceOrigin,
}

/// Where a library declaration's text comes from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SourceOrigin {
    /// Its sources: written by its authors, documentation included.
    Archive,
    /// Rendered from its class files — declarations only — for want of
    /// sources.
    Decompiled(crate::decompiled::MissingSources),
}

/// Where a library declaration is written — in its sources archive, else
/// in the declarations its class files record, rendered as source.
pub fn library_declaration_source(
    db: &dyn SourceDatabase,
    location: crate::ItemLocation,
) -> Option<LibrarySourceLocation> {
    let crate::ItemContainer::Library(package) = location.container else {
        return None;
    };
    let source = package.library(db).source(db).clone();
    let missing = match &source.sources {
        None => crate::decompiled::MissingSources::NoArchive,
        Some(sources) => {
            if let Some(found) = archive_declaration_source(db, location) {
                return Some(found);
            }
            crate::decompiled::MissingSources::NotInArchive {
                archive_name: file_name_of(sources),
            }
        }
    };
    decompiled_declaration_source(db, location, package, &source.archive, missing)
}

fn file_name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The archive directory of a library package's files:
/// `<module>/<package dirs>/`, `""` for the root package.
fn package_directory(db: &dyn SourceDatabase, package: LibraryPackage) -> String {
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
    if directory.is_empty() {
        directory
    } else {
        format!("{directory}/")
    }
}

/// The declaration in a file rendered from its class files: its top-level
/// class's — with the other declarations compiled from the same source
/// file (a Kotlin file's top-level functions) — under
/// `decompiled/<package>/<source file>`.
fn decompiled_declaration_source(
    db: &dyn SourceDatabase,
    location: crate::ItemLocation,
    package: LibraryPackage,
    compiled: &std::path::Path,
    missing: crate::decompiled::MissingSources,
) -> Option<LibrarySourceLocation> {
    use crate::decompiled::StubLanguage;
    let tree = library_package_tree(db, package);
    let chain = name_chain(&tree, location.item);
    let top_level = *chain.first()?;
    let language = StubLanguage::of(tree.item(top_level));
    let recorded_file = tree
        .source_files
        .iter()
        .find(|(index, _)| *index == top_level)
        .map(|(_, file)| file.to_string());
    let items: Vec<ItemId> = match &recorded_file {
        Some(file) => {
            let from_file: Vec<ItemId> = tree
                .source_files
                .iter()
                .filter(|(_, recorded)| **recorded == **file)
                .map(|(index, _)| *index)
                .collect();
            tree.top_level
                .iter()
                .copied()
                .filter(|item| from_file.contains(item))
                .filter(|item| StubLanguage::of(tree.item(*item)) == language)
                .collect()
        }
        None => vec![top_level],
    };
    let extension = language.extension();
    let file_name = recorded_file
        .filter(|file| file.ends_with(&format!(".{extension}")))
        .unwrap_or_else(|| format!("{}.{extension}", tree.item(top_level).name));
    let bodies = match language {
        StubLanguage::Java => compiled_bodies(&tree, &items, compiled),
        StubLanguage::Kotlin => crate::decompiled::CompiledBodies::default(),
    };
    let text: Arc<str> = Arc::from(crate::decompiled::render(
        &tree,
        &items,
        language,
        &file_name_of(compiled),
        &missing,
        &bodies,
    ));
    let entry = format!("decompiled/{}{file_name}", package_directory(db, package));
    let names = chain_names(&tree, &chain);
    let ranges = locate_in_text(&text, std::path::Path::new(&entry), &names, None)?;
    Some(LibrarySourceLocation {
        archive: compiled.to_path_buf(),
        entry,
        text,
        name_range: ranges.name_range,
        full_range: ranges.range,
        origin: SourceOrigin::Decompiled(missing),
    })
}

/// The decompiled bodies of Java classes' methods, their class files read
/// from the compiled archive (a JAR's; the JDK's `ct.sym` has no bodies).
fn compiled_bodies(
    tree: &ItemTree,
    items: &[ItemId],
    compiled: &std::path::Path,
) -> crate::decompiled::CompiledBodies {
    let Ok(mut archive) = classfile::Archive::open(compiled) else {
        return crate::decompiled::CompiledBodies::default();
    };
    crate::decompiled::CompiledBodies::of(tree, items, &mut |binary| {
        let index = archive.index_of(&format!("{binary}.class"))?;
        archive.read(index)
    })
}

/// Each item of a chain's name, and whether it is a classifier.
fn chain_names(tree: &ItemTree, chain: &[ItemId]) -> Vec<(Name, bool)> {
    chain
        .iter()
        .map(|index| {
            (
                tree.item(*index).name,
                tree.item(*index).kind.is_classifier(),
            )
        })
        .collect()
}

/// Where in a source file's `text` the item `names` leads to is (in
/// `package`, when given).
fn locate_in_text(
    text: &Arc<str>,
    path: &std::path::Path,
    names: &[(Name, bool)],
    package: Option<&intern::Path>,
) -> Option<crate::ItemRanges> {
    let language = syntax::Language::of_path(path)?;
    let parse = syntax::parse(text.clone(), language);
    let (source_tree, source_map) = match language {
        syntax::Language::Kotlin => crate::item_tree::lower_kotlin(&parse),
        syntax::Language::Java => crate::item_tree::lower_java(&parse),
    };
    if package.is_some_and(|package| source_tree.package != *package) {
        return None;
    }
    let found = find_by_names(&source_tree, names)?;
    Some(source_map.ranges(found))
}

/// Where a library declaration is written in its sources archive, by the
/// file it was compiled from (or, without that, by searching its package's
/// directory), then by its chain of names.
fn archive_declaration_source(
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
    let prefix = package_directory(db, package);
    let mut archive = classfile::Archive::open(sources).ok()?;
    let recorded_file = tree
        .source_files
        .iter()
        .find(|(index, _)| *index == top_level)
        .map(|(_, file)| file.to_string());
    let outermost = tree.item(top_level).name;
    let mut file_names: Vec<String> = recorded_file.into_iter().collect();
    for extension in ["java", "kt"] {
        file_names.push(format!("{outermost}.{extension}"));
    }
    let mut candidates: Vec<String> = file_names
        .iter()
        .map(|file| format!("{prefix}{file}"))
        .collect();
    // Then every source file of the package directory, then — for archives
    // laid out by source set (`commonMain/kotlin/…`, `jvmMain/…`, or
    // `generated/` as the standard library has them) — files of the same
    // name anywhere, those under a directory ending in the package's first.
    let mut elsewhere: Vec<(bool, String)> = Vec::new();
    for index in 0..archive.len() {
        let Some(name) = archive.name(index) else {
            continue;
        };
        let in_directory = name.strip_prefix(&prefix).is_some_and(|rest| {
            !rest.contains('/') && (rest.ends_with(".kt") || rest.ends_with(".java"))
        });
        if in_directory && !candidates.iter().any(|candidate| candidate == name) {
            candidates.push(name.to_owned());
            continue;
        }
        let file_name = name.rsplit('/').next().unwrap_or(name);
        if file_names.iter().any(|wanted| wanted == file_name)
            && !candidates.iter().any(|candidate| candidate == name)
        {
            let under_package = !prefix.is_empty() && name.contains(&format!("/{prefix}"));
            elsewhere.push((!under_package, name.to_owned()));
        }
    }
    // JVM sources before common ones: they hold the `actual` declarations.
    elsewhere.sort_by_key(|(not_under_package, name)| {
        (*not_under_package, !name.starts_with("jvm"), name.clone())
    });
    candidates.extend(elsewhere.into_iter().map(|(_, name)| name));
    let names = chain_names(&tree, &chain);
    if let Some(found) = find_in_entries(&mut archive, sources, candidates, &names, None) {
        return Some(found);
    }
    // The built-in classes are declared in files named otherwise
    // (`kotlin.Int` in `Primitives.kt`), some outside their package's
    // directory (`kotlin.collections.List` in `commonMain/kotlin/
    // Collections.kt`). Last, the files of the package's directory and of
    // the ones enclosing it, under any source set's root — those that
    // declare the package.
    let package_path = package.package(db);
    let package_directories = package_path
        .segments()
        .iter()
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>();
    let mut enclosing: Vec<String> = Vec::new();
    for depth in (1..=package_directories.len()).rev() {
        let ancestor = format!("{}/", package_directories[..depth].join("/"));
        for index in 0..archive.len() {
            let Some(name) = archive.name(index) else {
                continue;
            };
            let Some((directory, file)) = name.rsplit_once('/') else {
                continue;
            };
            let directory = format!("{directory}/");
            let in_ancestor = directory == ancestor || directory.ends_with(&format!("/{ancestor}"));
            let is_source = file.ends_with(".kt") || file.ends_with(".java");
            if in_ancestor && is_source && !enclosing.iter().any(|known| known == name) {
                enclosing.push(name.to_owned());
            }
        }
    }
    find_in_entries(&mut archive, sources, enclosing, &names, Some(package_path))
}

/// The first of `entries` that declares the item `names` leads to (in
/// `package`, when given), with where.
fn find_in_entries(
    archive: &mut classfile::Archive,
    sources: &std::path::Path,
    entries: Vec<String>,
    names: &[(Name, bool)],
    package: Option<&intern::Path>,
) -> Option<LibrarySourceLocation> {
    for entry in entries {
        let Some(index) = archive.index_of(&entry) else {
            continue;
        };
        let Some(bytes) = archive.read(index) else {
            continue;
        };
        let text: Arc<str> = Arc::from(String::from_utf8_lossy(&bytes).as_ref());
        let path = std::path::Path::new(&entry);
        if let Some(ranges) = locate_in_text(&text, path, names, package) {
            return Some(LibrarySourceLocation {
                archive: sources.to_path_buf(),
                entry,
                name_range: ranges.name_range,
                full_range: ranges.range,
                text,
                origin: SourceOrigin::Archive,
            });
        }
    }
    None
}

/// The items from the top-level one down to `item`.
fn name_chain(tree: &ItemTree, item: ItemId) -> Vec<ItemId> {
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
fn find_by_names(tree: &ItemTree, names: &[(Name, bool)]) -> Option<ItemId> {
    let mut level: &[ItemId] = &tree.top_level;
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
