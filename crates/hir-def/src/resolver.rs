//! Which declaration a written type name means, by the levels of
//! `docs/spec/resolution.md`: type parameters, nested classifiers of
//! enclosing classes (and of their supertypes), explicit imports, the file's
//! package, star imports, default imports. The first level with the name
//! wins; two candidates there make it ambiguous.

use base_db::{SourceDatabase, SourceFile};
use intern::{Name, Path};
use rustc_hash::FxHashMap;

use crate::def_map::{package_exists, visible_top_level};
use crate::item_tree::{ImportTarget, ItemId, Modifiers, Signature, TypeRef};
use crate::{ItemContainer, ItemLocation, file_item_source_map, file_item_tree, item_tree};

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

/// Whether `from` is built with the Android SDK on its classpath — by the
/// Android Gradle plugin, which generates classes (`R`, `BuildConfig`) a
/// build makes before compiling.
pub fn sees_android_sdk(db: &dyn SourceDatabase, from: SourceFile) -> bool {
    package_exists(db, from, &Path::parse("android.app"))
}

/// Whether `from` sees the packages default imports come from: the Kotlin
/// standard library's `kotlin` and the platform's `java.lang`. Without them
/// (a model guessed from the directory layout knows no libraries) any
/// simple name nothing resolves may still be a default import's.
pub fn default_imports_visible(db: &dyn SourceDatabase, from: SourceFile) -> bool {
    ["kotlin", "java.lang"]
        .iter()
        .all(|package| package_exists(db, from, &Path::parse(package)))
}

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

/// What the type name `path` written at `offset` in `file` refers to (in
/// a body, or wherever an editor asks).
pub fn resolve_type_path(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    offset: usize,
) -> Resolution {
    let tree = file_item_tree(db, file);
    let enclosing = file_item_source_map(db, file).items_enclosing(&tree, offset);
    resolve_type_path_within(db, file, path, &enclosing)
}

/// What the type name `path` written in the signature of `declaration`
/// (in `file`) refers to: it and the declarations around it are in scope.
/// Asks only the item tree, so an edit that moves declarations doesn't
/// change the answer's inputs.
pub fn resolve_type_path_in_declaration(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    declaration: ItemId,
) -> Resolution {
    let mut enclosing = file_item_tree(db, file).ancestors(declaration);
    enclosing.push(declaration);
    resolve_type_path_within(db, file, path, &enclosing)
}

/// What the type name `path` written in a body inside `innermost` (and the
/// items around it; at the top level of `file` without one) refers to. Asks
/// only the item tree, as [`resolve_type_path_in_declaration`] does: a
/// body's types don't depend on where in the file it is.
pub fn resolve_type_path_inside(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    innermost: Option<ItemId>,
) -> Resolution {
    match innermost {
        Some(item) => resolve_type_path_in_declaration(db, file, path, item),
        None => resolve_type_path_within(db, file, path, &[]),
    }
}

/// `path` resolved inside the items `enclosing` (outermost first).
fn resolve_type_path_within(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    enclosing: &[ItemId],
) -> Resolution {
    let Some((&first, rest)) = path.segments().split_first() else {
        return Resolution::Unresolved;
    };
    let mut walk = HierarchyWalk::new(db);
    let head = walk.resolve_simple_classifier(file, first, enclosing);
    match head {
        Some(Resolution::Declaration(location)) => walk.walk_nested(file, location, rest),
        Some(Resolution::TypeParameter { .. }) if rest.is_empty() => {
            head.unwrap_or(Resolution::Unresolved)
        }
        Some(Resolution::Ambiguous(_)) if rest.is_empty() => head.unwrap_or(Resolution::Unresolved),
        _ if !rest.is_empty() => walk.resolve_classifier_by_package(file, path),
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
            return walk_members(db, file, class, rest);
        }
    }
    if package_exists(db, file, path) {
        return Resolution::Package(path.clone());
    }
    Resolution::Unresolved
}

/// The top-level declarations of any kind named `name` that `file` sees,
/// level by level: explicit imports, the file's package, star imports,
/// Kotlin's default imports, the platform's. Expressions take the first
/// level where a candidate fits — which may not be the first non-empty one
/// (an imported `max(Int, Int)` doesn't stop `kotlin.comparisons.max` for
/// other arguments). Asked once per file and name, whichever body asks.
#[salsa::tracked(returns(clone), lru = 4096)]
pub fn top_level_levels(
    db: &dyn SourceDatabase,
    file: SourceFile,
    name: Name,
) -> std::sync::Arc<[Vec<ItemLocation>]> {
    let tree = file_item_tree(db, file);
    let mut levels = Vec::with_capacity(5);
    levels.push(
        tree.imports
            .iter()
            .filter(|import| import.visible_name() == Some(name))
            .flat_map(|import| resolve_import_path(db, file, &import.path).declarations())
            .collect(),
    );
    levels.push(visible_top_level(db, file, &tree.package, name));
    let mut from_star_imports = Vec::new();
    for import in tree.imports.iter() {
        if import.target == ImportTarget::Everything {
            from_star_imports.extend(visible_top_level(db, file, &import.path, name));
            from_star_imports.extend(class_star_import(db, file, &import.path, name));
        }
    }
    levels.push(from_star_imports);
    for level in [DEFAULT_IMPORT_PACKAGES, PLATFORM_DEFAULT_IMPORT_PACKAGES] {
        levels.push(
            level
                .iter()
                .flat_map(|package| visible_top_level(db, file, &Path::parse(package), name))
                .collect(),
        );
    }
    levels.into()
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

/// Whether the declaration is a class, interface, object or type alias.
pub fn is_classifier(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    item_tree(db, location.container)
        .item(location.item)
        .kind
        .is_classifier()
}

/// What `import a.Assertions.*` brings in named `name`, when the path is a
/// class: its static members (those its superclasses declare too) and its
/// nested classes.
fn class_star_import(
    db: &dyn SourceDatabase,
    file: SourceFile,
    path: &Path,
    name: Name,
) -> Vec<ItemLocation> {
    let Resolution::Declaration(class) = resolve_import_path(db, file, path) else {
        return Vec::new();
    };
    if !is_classifier(db, class) {
        return Vec::new();
    }
    let tree = item_tree(db, class.container);
    let own: Vec<ItemLocation> = tree
        .named(Some(class.item), name)
        .iter()
        .filter(|child| {
            let item = tree.item(**child);
            item.modifiers.contains(Modifiers::STATIC) || item.kind.is_classifier()
        })
        .map(|child| ItemLocation {
            container: class.container,
            item: *child,
        })
        .collect();
    if !own.is_empty() {
        return own;
    }
    HierarchyWalk::new(db).inherited_statics(file, class, name)
}

/// Follows `rest` from `class` through members of any kind (for imports:
/// `import a.Outer.Inner`, `import a.Registry.lookup`).
fn walk_members(
    db: &dyn SourceDatabase,
    file: SourceFile,
    class: ItemLocation,
    rest: &[Name],
) -> Resolution {
    let mut current = vec![class];
    for (position, name) in rest.iter().enumerate() {
        let is_last = position + 1 == rest.len();
        let Some(&container) = current.first() else {
            return Resolution::Unresolved;
        };
        let tree = item_tree(db, container.container);
        let mut members: Vec<ItemLocation> = tree
            .named(Some(container.item), *name)
            .iter()
            .filter(|child| is_last || tree.item(**child).kind.is_classifier())
            .map(|child| ItemLocation {
                container: container.container,
                item: *child,
            })
            .collect();
        // Java statics are inherited: `import ActivityCompat.checkSelfPermission`
        // names the method `ContextCompat` declares.
        if members.is_empty() && is_last {
            members = HierarchyWalk::new(db).inherited_statics(file, container, *name);
        }
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
) -> Vec<ItemLocation> {
    HierarchyWalk::new(db).nested_classifiers(from, class, name)
}

/// The class a supertype reference names: for a workspace class, resolved
/// where its header is written (outside its own body); for a library class,
/// by the fully qualified name it records, through `from`'s classpath.
pub fn resolve_supertype(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class: ItemLocation,
    supertype: &TypeRef,
) -> Option<ItemLocation> {
    HierarchyWalk::new(db).resolve_supertype(from, class, supertype)
}

/// A class named by its exact package and nesting, as compiled code
/// records it.
pub fn resolve_qualified(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &Path,
    classes: &[Name],
) -> Resolution {
    HierarchyWalk::new(db).resolve_qualified(from, package, classes)
}

/// A memo entry of one walk: a lookup that met itself again is part of a
/// supertype cycle, and finds nothing more there.
enum Visit<T> {
    Started,
    Finished(T),
}

/// One resolution's walk over class hierarchies. Each class's supertypes
/// are resolved once and each name is looked up in a class once, so a
/// diamond costs nothing extra and a cycle (`A : B`, `B : A` — an error,
/// but one that can be typed) ends.
///
/// Library classes resolve through the classpath of the file asking, so
/// the memo is keyed by that file too; a workspace class always resolves in
/// its own file.
struct HierarchyWalk<'db> {
    db: &'db dyn SourceDatabase,
    supertypes: FxHashMap<(SourceFile, ItemLocation), Visit<Vec<ItemLocation>>>,
    nested: FxHashMap<(SourceFile, ItemLocation, Name), Visit<Vec<ItemLocation>>>,
}

impl<'db> HierarchyWalk<'db> {
    fn new(db: &'db dyn SourceDatabase) -> Self {
        HierarchyWalk {
            db,
            supertypes: FxHashMap::default(),
            nested: FxHashMap::default(),
        }
    }

    /// The file whose classpath and imports resolve names for `class`.
    fn resolving_file(&self, from: SourceFile, class: ItemLocation) -> SourceFile {
        class.file().unwrap_or(from)
    }

    fn nested_classifiers(
        &mut self,
        from: SourceFile,
        class: ItemLocation,
        name: Name,
    ) -> Vec<ItemLocation> {
        let from = self.resolving_file(from, class);
        let key = (from, class, name);
        match self.nested.get(&key) {
            Some(Visit::Finished(found)) => return found.clone(),
            Some(Visit::Started) => return Vec::new(),
            None => {}
        }
        self.nested.insert(key, Visit::Started);
        let found = self.own_or_inherited_classifiers(from, class, name);
        self.nested.insert(key, Visit::Finished(found.clone()));
        found
    }

    fn own_or_inherited_classifiers(
        &mut self,
        from: SourceFile,
        class: ItemLocation,
        name: Name,
    ) -> Vec<ItemLocation> {
        let own = own_classifiers(self.db, class, name);
        if !own.is_empty() {
            return own;
        }
        let mut inherited = Vec::new();
        for super_class in self.supertypes(from, class) {
            inherited.extend(self.nested_classifiers(from, super_class, name));
        }
        inherited
    }

    /// The static members named `name` of `class`'s supertypes, nearest
    /// first: what Java lets a subclass's name reach.
    fn inherited_statics(
        &mut self,
        from: SourceFile,
        class: ItemLocation,
        name: Name,
    ) -> Vec<ItemLocation> {
        let mut found = Vec::new();
        let mut visited = vec![class];
        let mut frontier = vec![class];
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for current in frontier {
                for super_class in self.supertypes(from, current) {
                    if visited.contains(&super_class) {
                        continue;
                    }
                    visited.push(super_class);
                    let tree = item_tree(self.db, super_class.container);
                    let statics = tree
                        .named(Some(super_class.item), name)
                        .iter()
                        .filter(|child| tree.item(**child).modifiers.contains(Modifiers::STATIC))
                        .map(|child| ItemLocation {
                            container: super_class.container,
                            item: *child,
                        });
                    found.extend(statics);
                    next.push(super_class);
                }
            }
            frontier = next;
        }
        found
    }

    /// The classes `class` extends or implements, as far as they resolve.
    fn supertypes(&mut self, from: SourceFile, class: ItemLocation) -> Vec<ItemLocation> {
        let from = self.resolving_file(from, class);
        let key = (from, class);
        match self.supertypes.get(&key) {
            Some(Visit::Finished(found)) => return found.clone(),
            Some(Visit::Started) => return Vec::new(),
            None => {}
        }
        self.supertypes.insert(key, Visit::Started);
        let tree = item_tree(self.db, class.container);
        let mut found = Vec::new();
        if let Signature::Class { supertypes, .. } = &tree.item(class.item).signature {
            for supertype in supertypes.iter() {
                let resolved = self.resolve_supertype(from, class, supertype);
                if let Some(super_class) = resolved.filter(|super_class| *super_class != class) {
                    found.push(super_class);
                }
            }
        }
        self.supertypes.insert(key, Visit::Finished(found.clone()));
        found
    }

    fn resolve_supertype(
        &mut self,
        from: SourceFile,
        class: ItemLocation,
        supertype: &TypeRef,
    ) -> Option<ItemLocation> {
        if let TypeRef::Qualified {
            package, classes, ..
        } = supertype
        {
            let names: Vec<Name> = classes.iter().map(|segment| segment.name).collect();
            return match self.resolve_qualified(from, package, &names) {
                Resolution::Declaration(found) => Some(found),
                _ => None,
            };
        }
        let ItemContainer::File(file) = class.container else {
            return None;
        };
        let path = supertype.path()?;
        let header_scope = file_item_tree(self.db, file).ancestors(class.item);
        let (&first, rest) = path.segments().split_first()?;
        match self.resolve_simple_classifier(file, first, &header_scope)? {
            Resolution::Declaration(location) => match self.walk_nested(file, location, rest) {
                Resolution::Declaration(found) => Some(found),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_qualified(
        &mut self,
        from: SourceFile,
        package: &Path,
        classes: &[Name],
    ) -> Resolution {
        let Some((&outermost, rest)) = classes.split_first() else {
            return Resolution::Unresolved;
        };
        let candidates = classifiers_in_package(self.db, from, package, outermost);
        match Resolution::of_candidates(candidates) {
            Some(Resolution::Declaration(class)) => self.walk_nested(from, class, rest),
            Some(ambiguous) => ambiguous,
            None => Resolution::Unresolved,
        }
    }

    /// Follows `rest` from `class` through nested classifiers.
    fn walk_nested(&mut self, from: SourceFile, class: ItemLocation, rest: &[Name]) -> Resolution {
        let mut current = class;
        for name in rest {
            let found = self.nested_classifiers(from, current, *name);
            match Resolution::of_candidates(found) {
                Some(Resolution::Declaration(nested)) => current = nested,
                Some(ambiguous) => return ambiguous,
                None => return Resolution::Unresolved,
            }
        }
        Resolution::Declaration(current)
    }

    /// `a.b.C.D` read as package `a.b`, class `C`, nested `D` — the longest
    /// package prefix that has the class wins.
    fn resolve_classifier_by_package(&mut self, file: SourceFile, path: &Path) -> Resolution {
        let segments = path.segments();
        for package_length in (1..segments.len()).rev() {
            let package = Path::new(segments[..package_length].iter().copied());
            let classes = classifiers_in_package(self.db, file, &package, segments[package_length]);
            match Resolution::of_candidates(classes) {
                Some(Resolution::Declaration(class)) => {
                    return self.walk_nested(file, class, &segments[package_length + 1..]);
                }
                Some(ambiguous) => return ambiguous,
                None => {}
            }
        }
        if package_exists(self.db, file, path) {
            return Resolution::Package(path.clone());
        }
        Resolution::Unresolved
    }

    /// The classifier a single name means inside the items `enclosing`
    /// (outermost first), level by level.
    fn resolve_simple_classifier(
        &mut self,
        file: SourceFile,
        name: Name,
        enclosing: &[ItemId],
    ) -> Option<Resolution> {
        let db = self.db;
        let tree = file_item_tree(db, file);
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
            let found = self.nested_classifiers(file, class, name);
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
                from_star_imports.extend(self.nested_classifiers(file, class, name));
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
}

/// The classifiers named `name` declared directly in `class`.
fn own_classifiers(db: &dyn SourceDatabase, class: ItemLocation, name: Name) -> Vec<ItemLocation> {
    let tree = item_tree(db, class.container);
    tree.named(Some(class.item), name)
        .iter()
        .filter(|child| tree.item(**child).kind.is_classifier())
        .map(|child| ItemLocation {
            container: class.container,
            item: *child,
        })
        .collect()
}
