//! Written types ([`TypeRef`]) → [`Ty`]: names resolved where they're
//! written, type aliases expanded, Java types seen the way Kotlin sees them
//! (`java.lang.String` is `kotlin.String`, `int` is `kotlin.Int`, a Java
//! type's nullability is unknown).

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, Modifiers, Signature, TypeArgument, TypeRef};
use hir_def::resolver::{
    Resolution, resolve_qualified, resolve_supertype, resolve_type_path,
    resolve_type_path_in_declaration,
};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::{Name, Path};

use crate::ty::{ClassType, FunctionType, Nullability, Substitution, Ty, TypeParameterId};

/// Where a written type is read.
#[derive(Clone, Copy, Debug)]
pub struct TypeScope {
    /// For a type written in a workspace file, that file; for a library's,
    /// the file asking, whose classpath its fully qualified names resolve
    /// through.
    pub from: SourceFile,
    pub site: TypeSite,
    /// Declared in Java: nullability unknown.
    pub platform: bool,
}

/// Where in its file a type is written: what's in scope for its names.
#[derive(Clone, Copy, Debug)]
pub enum TypeSite {
    /// In a declaration's signature: it and the declarations around it (a
    /// library type names its type parameters only by name, declared by it
    /// or by a class around it).
    Declaration(ItemLocation),
    /// At an offset of the file's text: in a body.
    Offset(usize),
    /// In a body inside this item of the file (at its top level without
    /// one): what an offset of the file's text gives, without the offset.
    Inside(Option<hir_def::ItemId>),
}

impl TypeScope {
    /// A type written at `offset` in `file`'s text (inside a body, say).
    pub fn written_at(db: &dyn SourceDatabase, file: SourceFile, offset: usize) -> TypeScope {
        TypeScope {
            from: file,
            site: TypeSite::Offset(offset),
            platform: file.language(db) == syntax::Language::Java,
        }
    }

    /// The declaration whose signature holds the type, if it is in one.
    fn owner(self) -> Option<ItemLocation> {
        match self.site {
            TypeSite::Declaration(location) => Some(location),
            TypeSite::Offset(_) | TypeSite::Inside(_) => None,
        }
    }

    /// What `path` names here.
    fn resolve(self, db: &dyn SourceDatabase, path: &Path) -> Resolution {
        match self.site {
            TypeSite::Declaration(ItemLocation {
                container: ItemContainer::File(file),
                item,
            }) => resolve_type_path_in_declaration(db, file, path, item),
            // A compiled signature's names are fully qualified.
            TypeSite::Declaration(_) => resolve_type_path(db, self.from, path, 0),
            TypeSite::Offset(offset) => resolve_type_path(db, self.from, path, offset),
            TypeSite::Inside(innermost) => {
                hir_def::resolver::resolve_type_path_inside(db, self.from, path, innermost)
            }
        }
    }

    /// A type written in a body of `file` inside `innermost` (see
    /// [`TypeSite::Inside`]).
    pub fn inside(
        db: &dyn SourceDatabase,
        file: SourceFile,
        innermost: Option<hir_def::ItemId>,
    ) -> TypeScope {
        TypeScope {
            from: file,
            site: TypeSite::Inside(innermost),
            platform: file.language(db) == syntax::Language::Java,
        }
    }

    /// Where the types in `location`'s signature are read.
    pub fn of_declaration(
        db: &dyn SourceDatabase,
        from: SourceFile,
        location: ItemLocation,
    ) -> TypeScope {
        match location.container {
            ItemContainer::File(file) => TypeScope {
                from: file,
                site: TypeSite::Declaration(location),
                platform: file.language(db) == syntax::Language::Java,
            },
            ItemContainer::Library(_) => {
                let tree = item_tree(db, location.container);
                let item = tree.item(location.item);
                TypeScope {
                    from,
                    site: TypeSite::Declaration(location),
                    platform: item.modifiers.contains(Modifiers::PLATFORM),
                }
            }
        }
    }
}

/// Java's types Kotlin sees as its own.
const MAPPED_TYPES: &[(&str, &str)] = &[
    ("java.lang.Object", "kotlin.Any"),
    ("java.lang.String", "kotlin.String"),
    ("java.lang.CharSequence", "kotlin.CharSequence"),
    ("java.lang.Throwable", "kotlin.Throwable"),
    ("java.lang.Cloneable", "kotlin.Cloneable"),
    ("java.lang.Number", "kotlin.Number"),
    ("java.lang.Comparable", "kotlin.Comparable"),
    ("java.lang.Enum", "kotlin.Enum"),
    ("java.lang.annotation.Annotation", "kotlin.Annotation"),
    ("java.lang.Integer", "kotlin.Int"),
    ("java.lang.Long", "kotlin.Long"),
    ("java.lang.Short", "kotlin.Short"),
    ("java.lang.Byte", "kotlin.Byte"),
    ("java.lang.Character", "kotlin.Char"),
    ("java.lang.Boolean", "kotlin.Boolean"),
    ("java.lang.Float", "kotlin.Float"),
    ("java.lang.Double", "kotlin.Double"),
    ("java.lang.Iterable", "kotlin.collections.MutableIterable"),
    ("java.util.Iterator", "kotlin.collections.MutableIterator"),
    (
        "java.util.ListIterator",
        "kotlin.collections.MutableListIterator",
    ),
    (
        "java.util.Collection",
        "kotlin.collections.MutableCollection",
    ),
    ("java.util.List", "kotlin.collections.MutableList"),
    ("java.util.Set", "kotlin.collections.MutableSet"),
    ("java.util.Map", "kotlin.collections.MutableMap"),
    (
        "java.util.Map.Entry",
        "kotlin.collections.MutableMap.MutableEntry",
    ),
];

const PRIMITIVES: &[(&str, &str)] = &[
    ("int", "Int"),
    ("long", "Long"),
    ("short", "Short"),
    ("byte", "Byte"),
    ("char", "Char"),
    ("boolean", "Boolean"),
    ("float", "Float"),
    ("double", "Double"),
    ("void", "Unit"),
];

/// How deep type alias expansion goes.
const MAX_ALIAS_DEPTH: usize = 8;

/// A package the analysis names itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KnownPackage {
    Kotlin,
    KotlinCollections,
    KotlinEnums,
    KotlinReflect,
}

impl KnownPackage {
    const ALL: [KnownPackage; 4] = [
        KnownPackage::Kotlin,
        KnownPackage::KotlinCollections,
        KnownPackage::KotlinEnums,
        KnownPackage::KotlinReflect,
    ];

    fn spelled(self) -> &'static str {
        match self {
            KnownPackage::Kotlin => "kotlin",
            KnownPackage::KotlinCollections => "kotlin.collections",
            KnownPackage::KotlinEnums => "kotlin.enums",
            KnownPackage::KotlinReflect => "kotlin.reflect",
        }
    }

    /// The package as a path, parsed once.
    pub fn path(self) -> &'static Path {
        static PATHS: std::sync::OnceLock<[Path; 4]> = std::sync::OnceLock::new();
        let paths =
            PATHS.get_or_init(|| KnownPackage::ALL.map(|known| Path::parse(known.spelled())));
        &paths[self as usize]
    }
}

/// A class's names, outermost first: most types name one or two.
type ClassNames = smallvec::SmallVec<[Name; 4]>;

/// The class `<package>.<name>` (`kotlin.Int`, `kotlin.Map.Entry`), seen
/// from `from`.
pub fn builtin(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: KnownPackage,
    name: &str,
    arguments: Vec<Ty>,
) -> Ty {
    let names: ClassNames = name.split('.').map(Name::new).collect();
    match resolve_qualified(db, from, package.path(), &names) {
        Resolution::Declaration(class) => Ty::Class(ClassType {
            class,
            arguments: arguments.into_boxed_slice(),
            nullability: Nullability::NotNull,
        }),
        _ => Ty::Unknown,
    }
}

/// `type_ref` as a type.
pub fn lower_type(db: &dyn SourceDatabase, scope: TypeScope, type_ref: &TypeRef) -> Ty {
    lower(db, scope, type_ref, 0)
}

fn lower(db: &dyn SourceDatabase, scope: TypeScope, type_ref: &TypeRef, depth: usize) -> Ty {
    match type_ref {
        TypeRef::Named { segments, nullable } => {
            let nullability = nullability(scope, *nullable);
            let path = Path::new(segments.iter().map(|segment| segment.name));
            let arguments = segments
                .last()
                .map(|segment| lower_arguments(db, scope, &segment.arguments, depth))
                .unwrap_or_default();
            match scope.resolve(db, &path) {
                Resolution::Declaration(class) => {
                    class_type(db, scope, class, arguments, nullability, depth)
                }
                Resolution::TypeParameter { owner, index } => Ty::TypeParameter {
                    parameter: TypeParameterId {
                        owner,
                        index: index as u32,
                    },
                    nullability,
                },
                _ => Ty::Error,
            }
        }
        TypeRef::Qualified {
            package,
            classes,
            nullable,
        } => {
            let nullability = nullability(scope, *nullable);
            let arguments = classes
                .last()
                .map(|segment| lower_arguments(db, scope, &segment.arguments, depth))
                .unwrap_or_default();
            let names: ClassNames = classes.iter().map(|segment| segment.name).collect();
            match resolve_qualified(db, scope.from, package, &names) {
                Resolution::Declaration(class) => {
                    class_type(db, scope, class, arguments, nullability, depth)
                }
                _ => Ty::Error,
            }
        }
        TypeRef::TypeParameter { name, nullable } => match type_parameter_named(db, scope, *name) {
            Some(parameter) => Ty::TypeParameter {
                parameter,
                nullability: nullability(scope, *nullable),
            },
            None => Ty::Error,
        },
        TypeRef::Function {
            receiver,
            parameters,
            result,
            suspend,
            nullable,
        } => Ty::Function(Box::new(FunctionType {
            receiver: receiver
                .as_ref()
                .map(|receiver| lower(db, scope, receiver, depth)),
            parameters: parameters
                .iter()
                .map(|parameter| lower(db, scope, parameter, depth))
                .collect(),
            result: lower(db, scope, result, depth),
            suspend: *suspend,
            nullability: Nullability::of(*nullable),
        })),
        TypeRef::Primitive { name } => {
            let kotlin = PRIMITIVES
                .iter()
                .find(|(java, _)| *java == name.as_str())
                .map(|(_, kotlin)| *kotlin);
            match kotlin {
                Some(kotlin) => builtin(
                    db,
                    scope.from,
                    crate::lower::KnownPackage::Kotlin,
                    kotlin,
                    Vec::new(),
                ),
                None => Ty::Unknown,
            }
        }
        TypeRef::Array { element } => {
            if let TypeRef::Primitive { name } = element.as_ref() {
                let array = PRIMITIVES
                    .iter()
                    .find(|(java, _)| *java == name.as_str())
                    .map(|(_, kotlin)| format!("{kotlin}Array"));
                return match array {
                    Some(array) => builtin(
                        db,
                        scope.from,
                        crate::lower::KnownPackage::Kotlin,
                        &array,
                        Vec::new(),
                    )
                    .with_nullability(nullability(scope, false)),
                    None => Ty::Unknown,
                };
            }
            let element = lower(db, scope, element, depth);
            builtin(
                db,
                scope.from,
                crate::lower::KnownPackage::Kotlin,
                "Array",
                vec![element],
            )
            .with_nullability(nullability(scope, false))
        }
        TypeRef::Unreadable => Ty::Error,
    }
}

/// A supertype as written in `class`'s header, as a class type. A
/// workspace supertype's name resolves where the header is (outside the
/// class's own body).
pub fn lower_supertype(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class: ItemLocation,
    supertype: &TypeRef,
) -> Option<ClassType> {
    let scope = TypeScope::of_declaration(db, from, class);
    let TypeRef::Named { segments, .. } = supertype else {
        return lower(db, scope, supertype, 0).class().cloned();
    };
    let location = resolve_supertype(db, from, class, supertype)?;
    let arguments = segments
        .last()
        .map(|segment| lower_arguments(db, scope, &segment.arguments, 0))
        .unwrap_or_default();
    let scope = TypeScope {
        platform: false,
        ..scope
    };
    class_type(db, scope, location, arguments, Nullability::NotNull, 0)
        .class()
        .cloned()
}

fn nullability(scope: TypeScope, nullable: bool) -> Nullability {
    match (nullable, scope.platform) {
        (true, _) => Nullability::Nullable,
        (false, true) => Nullability::Platform,
        (false, false) => Nullability::NotNull,
    }
}

fn lower_arguments(
    db: &dyn SourceDatabase,
    scope: TypeScope,
    arguments: &[TypeArgument],
    depth: usize,
) -> Vec<Ty> {
    arguments
        .iter()
        .map(|argument| match argument {
            TypeArgument::Star => Ty::Unknown,
            TypeArgument::Type { type_ref, .. } => lower(db, scope, type_ref, depth),
        })
        .collect()
}

/// A class type, a type alias expanded.
fn class_type(
    db: &dyn SourceDatabase,
    scope: TypeScope,
    class: ItemLocation,
    arguments: Vec<Ty>,
    nullability: Nullability,
    depth: usize,
) -> Ty {
    let class = kotlin_view(db, scope.from, class);
    let tree = item_tree(db, class.container);
    let item = tree.item(class.item);
    // `kotlin.Nothing` written in a signature (`fun fail(): Nothing`):
    // the type of what never completes.
    let is_nothing = item.parent.is_none()
        && item.name.as_str() == "Nothing"
        && tree.package.to_string() == "kotlin";
    if is_nothing {
        return Ty::Nothing {
            nullable: nullability == Nullability::Nullable,
        };
    }
    if item.kind == ItemKind::TypeAlias {
        if depth >= MAX_ALIAS_DEPTH {
            return Ty::Unknown;
        }
        let Signature::TypeAlias { target } = &item.signature else {
            return Ty::Unknown;
        };
        let alias_scope = TypeScope::of_declaration(db, scope.from, class);
        let expanded = lower(db, alias_scope, target, depth + 1);
        let mut substitution = Substitution::default();
        for (index, argument) in arguments
            .into_iter()
            .enumerate()
            .take(item.type_parameters.len())
        {
            substitution.insert(
                TypeParameterId {
                    owner: class,
                    index: index as u32,
                },
                argument,
            );
        }
        let expanded = expanded.substitute(&substitution);
        return match nullability {
            Nullability::Nullable => expanded.nullable(),
            _ => expanded,
        };
    }
    Ty::Class(ClassType {
        class,
        arguments: arguments.into_boxed_slice(),
        nullability,
    })
}

/// The type parameter `name` declared by the scope's owner or a
/// declaration around it.
fn type_parameter_named(
    db: &dyn SourceDatabase,
    scope: TypeScope,
    name: Name,
) -> Option<TypeParameterId> {
    let owner = scope.owner()?;
    let tree = item_tree(db, owner.container);
    let mut current = Some(owner.item);
    while let Some(index) = current {
        let item = tree.item(index);
        if let Some(position) = item
            .type_parameters
            .iter()
            .position(|parameter| parameter.name == name)
        {
            return Some(TypeParameterId {
                owner: ItemLocation {
                    container: owner.container,
                    item: index,
                },
                index: position as u32,
            });
        }
        current = item.parent;
    }
    None
}

/// The Kotlin class a Java class is seen as (`java.lang.String` →
/// `kotlin.String`), or the class itself.
fn kotlin_view(db: &dyn SourceDatabase, from: SourceFile, class: ItemLocation) -> ItemLocation {
    let tree = item_tree(db, class.container);
    let is_java_package = tree
        .package
        .segments()
        .first()
        .is_some_and(|first| first.as_str() == "java");
    if !is_java_package {
        return class;
    }
    let mapped = mapped_types().iter().find(|mapped| {
        mapped.java.package == tree.package && is_named(&tree, class.item, &mapped.java.classes)
    });
    let Some(mapped) = mapped else {
        return class;
    };
    match resolve_qualified(db, from, &mapped.kotlin.package, &mapped.kotlin.classes) {
        Resolution::Declaration(kotlin) => kotlin,
        _ => class,
    }
}

/// A class's qualified name: its package, and its class and the classes
/// around it, outermost first.
#[derive(Debug, PartialEq, Eq)]
struct ClassName {
    package: Path,
    classes: Box<[Name]>,
}

impl ClassName {
    /// `kotlin.collections.MutableMap.MutableEntry` → package and classes,
    /// by the convention that classes start with a capital letter.
    fn parse(qualified: &str) -> ClassName {
        let segments: Vec<&str> = qualified.split('.').collect();
        let first_class = segments
            .iter()
            .position(|segment| segment.starts_with(|first: char| first.is_uppercase()))
            .unwrap_or(segments.len());
        ClassName {
            package: Path::new(
                segments[..first_class]
                    .iter()
                    .map(|segment| Name::new(segment)),
            ),
            classes: segments[first_class..]
                .iter()
                .map(|segment| Name::new(segment))
                .collect(),
        }
    }
}

/// A Java type Kotlin sees as its own, as names.
struct MappedType {
    java: ClassName,
    kotlin: ClassName,
}

/// [`MAPPED_TYPES`] as names, read once.
fn mapped_types() -> &'static [MappedType] {
    static MAPPED: std::sync::OnceLock<Box<[MappedType]>> = std::sync::OnceLock::new();
    MAPPED.get_or_init(|| {
        MAPPED_TYPES
            .iter()
            .map(|(java, kotlin)| MappedType {
                java: ClassName::parse(java),
                kotlin: ClassName::parse(kotlin),
            })
            .collect()
    })
}

/// Whether `item` and the classes around it are named `classes`
/// (outermost first), and nothing is around the outermost.
fn is_named(tree: &hir_def::item_tree::ItemTree, item: hir_def::ItemId, classes: &[Name]) -> bool {
    let mut current = Some(item);
    for expected in classes.iter().rev() {
        let Some(index) = current else {
            return false;
        };
        let found = tree.item(index);
        if found.name != *expected {
            return false;
        }
        current = found.parent;
    }
    current.is_none()
}

#[cfg(test)]
mod tests {
    use base_db::fixture::Fixture;

    use super::*;

    #[test]
    fn known_packages_are_their_spelling_parsed_once() {
        for known in KnownPackage::ALL {
            assert_eq!(*known.path(), Path::parse(known.spelled()), "{known:?}");
            assert!(std::ptr::eq(known.path(), known.path()));
        }
    }

    #[test]
    fn a_mapped_types_name_is_its_package_and_its_classes() {
        assert_eq!(
            ClassName::parse("java.util.Map.Entry"),
            ClassName {
                package: Path::parse("java.util"),
                classes: [Name::new("Map"), Name::new("Entry")].into(),
            }
        );
        let kotlin = &mapped_types()
            .iter()
            .find(|mapped| mapped.java == ClassName::parse("java.util.Map.Entry"))
            .unwrap()
            .kotlin;
        assert_eq!(kotlin.package, Path::parse("kotlin.collections"));
        assert_eq!(
            &*kotlin.classes,
            [Name::new("MutableMap"), Name::new("MutableEntry")]
        );
    }

    #[test]
    fn a_class_is_named_by_its_whole_nesting_and_nothing_more() {
        let fixture = Fixture::parse(
            r#"
//- /src/java/util/Map.java
package java.util;
public interface Map<K, V> { interface Entry<K, V> {} }
//- /src/java/util/Entry.java
package java.util;
public class Entry {}
"#,
        );
        let db = &fixture.database;
        let tree_of = |path: &str| {
            let file = db.source_file(fixture.file(path)).unwrap();
            hir_def::file_item_tree(db, file)
        };
        let map_file = tree_of("/src/java/util/Map.java");
        let find = |tree: &hir_def::item_tree::ItemTree, name: &str| {
            tree.indexed_items()
                .find(|(_, item)| item.name.as_str() == name)
                .map(|(id, _)| id)
                .unwrap()
        };
        let map_entry = [Name::new("Map"), Name::new("Entry")];
        let nested = find(&map_file, "Entry");
        let map = find(&map_file, "Map");
        assert!(is_named(&map_file, nested, &map_entry));
        assert!(!is_named(&map_file, map, &map_entry));
        assert!(
            !is_named(&map_file, nested, &[Name::new("Entry")]),
            "it has an outer class"
        );
        let entry_file = tree_of("/src/java/util/Entry.java");
        let top_level = find(&entry_file, "Entry");
        assert!(!is_named(&entry_file, top_level, &map_entry));
        assert!(is_named(&entry_file, top_level, &[Name::new("Entry")]));
    }
}
