//! Which declaration a written type name means, by the levels of
//! `docs/spec/resolution.md`: type parameters, nested classifiers of
//! enclosing classes (and of their supertypes), explicit imports, the file's
//! package, star imports, default imports. The first level with the name
//! wins; two candidates there make it ambiguous.

use base_db::{SourceDatabase, SourceFile};
use intern::{Name, Path};

use crate::def_map::{package_exists, visible_top_level};
use crate::item_tree::{ImportTarget, ItemIndex, Signature, TypeRef};
use crate::{ItemContainer, ItemLocation, file_item_tree, item_tree};

/// Kotlin's own default imports.
pub const DEFAULT_IMPORT_PACKAGES: &[&str] = &[
    "kotlin",
    "kotlin.annotation",
    "kotlin.collections",
    "kotlin.comparisons",
    "kotlin.io",
    "kotlin.ranges",
    "kotlin.sequences",
    "kotlin.text",
];

/// The JVM platform's default imports, one level below Kotlin's own:
/// `String` is `kotlin.String`, not `java.lang.String`.
pub const PLATFORM_DEFAULT_IMPORT_PACKAGES: &[&str] = &["java.lang", "kotlin.jvm"];

/// How far supertypes are followed looking for a nested classifier.
const MAX_SUPERTYPE_DEPTH: usize = 16;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolution {
    /// Exactly one declaration.
    Declaration(ItemLocation),
    /// The `index`-th type parameter of `owner`.
    TypeParameter { owner: ItemLocation, index: usize },
    /// Several declarations at the level that has the name.
    Ambiguous(Vec<ItemLocation>),
    /// A package, as the qualifier of a qualified name.
    Package(Path),
    /// Nothing visible has the name.
    Unresolved,
}

impl Resolution {
    fn of_candidates(candidates: Vec<ItemLocation>) -> Option<Resolution> {
        let mut unique: Vec<ItemLocation> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if !unique.contains(&candidate) {
                unique.push(candidate);
            }
        }
        let candidates = unique;
        match candidates.len() {
            0 => None,
            1 => Some(Resolution::Declaration(candidates[0])),
            _ => Some(Resolution::Ambiguous(candidates)),
        }
    }

    /// The declarations it names: one, several, or none.
    pub fn declarations(&self) -> Vec<ItemLocation> {
        match self {
            Resolution::Declaration(location) => vec![*location],
            Resolution::Ambiguous(locations) => locations.clone(),
            _ => Vec::new(),
        }
    }
}

/// What the type name `path` written at `offset` in `file` refers to.
pub fn resolve_type_path(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    offset: usize,
) -> Resolution {
    let Some((&first, rest)) = path.segments().split_first() else {
        return Resolution::Unresolved;
    };
    let head = resolve_simple_classifier(db, file, first, offset, 0);
    match head {
        Some(Resolution::Declaration(location)) => walk_nested(db, file, location, rest, 0),
        Some(Resolution::TypeParameter { .. }) if rest.is_empty() => {
            head.unwrap_or(Resolution::Unresolved)
        }
        Some(Resolution::Ambiguous(_)) if rest.is_empty() => head.unwrap_or(Resolution::Unresolved),
        _ if !rest.is_empty() => resolve_classifier_by_package(db, file, path),
        _ => Resolution::Unresolved,
    }
}

/// What an import's path names: a package, or declarations — top-level
/// ones of a package, or nested ones of a class (`a.b.Outer.Inner`), of any
/// kind (an import can name a function or a property).
pub fn resolve_import_path(db: &dyn SourceDatabase, file: SourceFile, path: &Path) -> Resolution {
    let segments = path.segments();
    for package_length in (0..segments.len()).rev() {
        let package = Path::new(segments[..package_length].iter().copied());
        let name = segments[package_length];
        let rest = &segments[package_length + 1..];
        let top_level = visible_top_level(db, file, &package, name);
        if top_level.is_empty() {
            continue;
        }
        if rest.is_empty() {
            return Resolution::of_candidates(top_level).unwrap_or(Resolution::Unresolved);
        }
        let classes: Vec<ItemLocation> = top_level
            .into_iter()
            .filter(|location| is_classifier(db, *location))
            .collect();
        if let Some(Resolution::Declaration(class)) = Resolution::of_candidates(classes) {
            return walk_members(db, class, rest);
        }
    }
    if package_exists(db, file, path) {
        return Resolution::Package(path.clone());
    }
    Resolution::Unresolved
}

/// `a.b.C.D` read as package `a.b`, class `C`, nested `D` — the longest
/// package prefix that has the class wins.
fn resolve_classifier_by_package(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
) -> Resolution {
    let segments = path.segments();
    for package_length in (1..segments.len()).rev() {
        let package = Path::new(segments[..package_length].iter().copied());
        let classes: Vec<ItemLocation> =
            visible_top_level(db, file, &package, segments[package_length])
                .into_iter()
                .filter(|location| is_classifier(db, *location))
                .collect();
        match Resolution::of_candidates(classes) {
            Some(Resolution::Declaration(class)) => {
                return walk_nested(db, file, class, &segments[package_length + 1..], 0);
            }
            Some(ambiguous) => return ambiguous,
            None => {}
        }
    }
    if package_exists(db, file, path) {
        return Resolution::Package(path.clone());
    }
    Resolution::Unresolved
}

/// The classifier a single name means at `offset`, level by level.
fn resolve_simple_classifier(
    db: &dyn SourceDatabase,
    file: SourceFile,
    name: Name,
    offset: usize,
    depth: usize,
) -> Option<Resolution> {
    let tree = file_item_tree(db, file);
    let enclosing = tree.items_enclosing(offset);

    for index in enclosing.iter().rev() {
        let item = tree.item(*index);
        if let Some(position) = item
            .type_parameters
            .iter()
            .position(|parameter| parameter.name == name)
        {
            return Some(Resolution::TypeParameter {
                owner: ItemLocation::in_file(file, *index),
                index: position,
            });
        }
    }

    for index in enclosing.iter().rev() {
        if !tree.item(*index).kind.is_classifier() {
            continue;
        }
        let class = ItemLocation::in_file(file, *index);
        let found = nested_classifiers(db, file, class, name, depth);
        if let Some(resolution) = Resolution::of_candidates(found) {
            return Some(resolution);
        }
    }

    let imported: Vec<ItemLocation> = tree
        .imports
        .iter()
        .filter(|import| import.visible_name() == Some(name))
        .flat_map(|import| resolve_import_path(db, file, &import.path).declarations())
        .filter(|location| is_classifier(db, *location))
        .collect();
    if let Some(resolution) = Resolution::of_candidates(imported) {
        return Some(resolution);
    }

    let same_package = classifiers_in_package(db, file, &tree.package, name);
    if let Some(resolution) = Resolution::of_candidates(same_package) {
        return Some(resolution);
    }

    let mut from_star_imports = Vec::new();
    for import in tree.imports.iter() {
        if import.target != ImportTarget::Everything {
            continue;
        }
        if package_exists(db, file, &import.path) {
            from_star_imports.extend(classifiers_in_package(db, file, &import.path, name));
            continue;
        }
        // `import a.b.Outer.*` imports Outer's nested classifiers.
        if let Resolution::Declaration(class) = resolve_import_path(db, file, &import.path) {
            from_star_imports.extend(nested_classifiers(db, file, class, name, depth));
        }
    }
    if let Some(resolution) = Resolution::of_candidates(from_star_imports) {
        return Some(resolution);
    }

    for level in [DEFAULT_IMPORT_PACKAGES, PLATFORM_DEFAULT_IMPORT_PACKAGES] {
        let defaults: Vec<ItemLocation> = level
            .iter()
            .flat_map(|package| classifiers_in_package(db, file, &Path::parse(package), name))
            .collect();
        if let Some(resolution) = Resolution::of_candidates(defaults) {
            return Some(resolution);
        }
    }
    None
}

fn classifiers_in_package(
    db: &dyn SourceDatabase,
    file: SourceFile,
    package: &Path,
    name: Name,
) -> Vec<ItemLocation> {
    visible_top_level(db, file, package, name)
        .into_iter()
        .filter(|location| is_classifier(db, *location))
        .collect()
}

fn is_classifier(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    item_tree(db, location.container)
        .item(location.item)
        .kind
        .is_classifier()
}

/// Follows `rest` from `class` through nested classifiers.
fn walk_nested(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class: ItemLocation,
    rest: &[Name],
    depth: usize,
) -> Resolution {
    let mut current = class;
    for name in rest {
        match Resolution::of_candidates(nested_classifiers(db, from, current, *name, depth)) {
            Some(Resolution::Declaration(nested)) => current = nested,
            Some(ambiguous) => return ambiguous,
            None => return Resolution::Unresolved,
        }
    }
    Resolution::Declaration(current)
}

/// Follows `rest` from `class` through members of any kind (for imports:
/// `import a.Outer.Inner`, `import a.Registry.lookup`).
fn walk_members(db: &dyn SourceDatabase, class: ItemLocation, rest: &[Name]) -> Resolution {
    let mut current = vec![class];
    for (position, name) in rest.iter().enumerate() {
        let is_last = position + 1 == rest.len();
        let Some(&container) = current.first() else {
            return Resolution::Unresolved;
        };
        let tree = item_tree(db, container.container);
        let members: Vec<ItemLocation> = tree
            .item(container.item)
            .children
            .iter()
            .filter(|child| tree.item(**child).name == *name)
            .filter(|child| is_last || tree.item(**child).kind.is_classifier())
            .map(|child| ItemLocation {
                container: container.container,
                item: *child,
            })
            .collect();
        if members.is_empty() {
            return Resolution::Unresolved;
        }
        current = members;
    }
    Resolution::of_candidates(current).unwrap_or(Resolution::Unresolved)
}

/// The classifiers named `name` nested in `class`, or else in its
/// supertypes (Kotlin sees a supertype's nested classes by their simple
/// name inside the subclass). `from` is the file asking: library types
/// resolve through its classpath.
pub fn nested_classifiers(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class: ItemLocation,
    name: Name,
    depth: usize,
) -> Vec<ItemLocation> {
    let tree = item_tree(db, class.container);
    let item = tree.item(class.item);
    let own: Vec<ItemLocation> = item
        .children
        .iter()
        .filter(|child| {
            let child = tree.item(**child);
            child.name == name && child.kind.is_classifier()
        })
        .map(|child| ItemLocation {
            container: class.container,
            item: *child,
        })
        .collect();
    if !own.is_empty() || depth >= MAX_SUPERTYPE_DEPTH {
        return own;
    }
    let Signature::Class { supertypes, .. } = &item.signature else {
        return Vec::new();
    };
    let mut inherited = Vec::new();
    for supertype in supertypes.iter() {
        let Some(super_class) = resolve_supertype(db, from, class, supertype, depth) else {
            continue;
        };
        if super_class == class {
            continue;
        }
        inherited.extend(nested_classifiers(db, from, super_class, name, depth + 1));
    }
    inherited
}

/// The class a supertype reference names: for a workspace class, resolved
/// where its header is written (outside its own body); for a library class,
/// by the fully qualified name it records, through `from`'s classpath.
pub fn resolve_supertype(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class: ItemLocation,
    supertype: &TypeRef,
    depth: usize,
) -> Option<ItemLocation> {
    if let TypeRef::Qualified {
        package, classes, ..
    } = supertype
    {
        let names: Vec<Name> = classes.iter().map(|segment| segment.name).collect();
        return match resolve_qualified(db, from, package, &names, depth) {
            Resolution::Declaration(found) => Some(found),
            _ => None,
        };
    }
    let ItemContainer::File(file) = class.container else {
        return None;
    };
    let path = supertype.path()?;
    let tree = file_item_tree(db, file);
    let header = header_offset(&tree, class.item);
    let (&first, rest) = path.segments().split_first()?;
    let head = resolve_simple_classifier(db, file, first, header, depth + 1)?;
    match head {
        Resolution::Declaration(location) => {
            match walk_nested(db, file, location, rest, depth + 1) {
                Resolution::Declaration(found) => Some(found),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A class named by its exact package and nesting, as compiled code
/// records it.
pub fn resolve_qualified(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &Path,
    classes: &[Name],
    depth: usize,
) -> Resolution {
    let Some((&outermost, rest)) = classes.split_first() else {
        return Resolution::Unresolved;
    };
    let candidates = classifiers_in_package(db, from, package, outermost);
    match Resolution::of_candidates(candidates) {
        Some(Resolution::Declaration(class)) => walk_nested(db, from, class, rest, depth),
        Some(ambiguous) => ambiguous,
        None => Resolution::Unresolved,
    }
}

/// An offset inside the item's parent but outside the item itself — where
/// its header's names resolve.
fn header_offset(tree: &crate::ItemTree, item: ItemIndex) -> usize {
    tree.item(item).range.start.saturating_sub(1) as usize
}
