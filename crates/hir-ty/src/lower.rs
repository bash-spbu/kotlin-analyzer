//! Written types ([`TypeRef`]) → [`Ty`]: names resolved where they're
//! written, type aliases expanded, Java types seen the way Kotlin sees them
//! (`java.lang.String` is `kotlin.String`, `int` is `kotlin.Int`, a Java
//! type's nullability is unknown).

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, Modifiers, Signature, TypeArgument, TypeRef};
use hir_def::resolver::{Resolution, resolve_qualified, resolve_supertype, resolve_type_path};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::{Name, Path};

use crate::ty::{ClassType, FunctionType, Nullability, Substitution, Ty, TypeParameterId};

/// Where a written type is read.
#[derive(Clone, Copy, Debug)]
pub struct TypeScope {
    /// For a type written in a workspace file, that file (names resolve at
    /// the type's own position there); for a library's, the file asking,
    /// whose classpath its fully qualified names resolve through.
    pub from: SourceFile,
    /// The declaration whose signature holds the type: a library type
    /// names its type parameters only by name, declared by it or by a class
    /// around it.
    pub owner: Option<ItemLocation>,
    /// Declared in Java: nullability unknown.
    pub platform: bool,
}

impl TypeScope {
    /// Where the types in `location`'s signature are read.
    /// A type written in `file`'s text (inside a body, say).
    pub fn written_in(db: &dyn SourceDatabase, file: SourceFile) -> TypeScope {
        TypeScope {
            from: file,
            owner: None,
            platform: file.language(db) == syntax::Language::Java,
        }
    }

    pub fn of_declaration(
        db: &dyn SourceDatabase,
        from: SourceFile,
        location: ItemLocation,
    ) -> TypeScope {
        match location.container {
            ItemContainer::File(file) => TypeScope {
                from: file,
                owner: Some(location),
                platform: file.language(db) == syntax::Language::Java,
            },
            ItemContainer::Library(_) => {
                let tree = item_tree(db, location.container);
                let item = tree.item(location.item);
                TypeScope {
                    from,
                    owner: Some(location),
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

/// The class `kotlin.<name>` (or another package's), seen from `from`.
pub fn builtin(
    db: &dyn SourceDatabase,
    from: SourceFile,
    package: &str,
    name: &str,
    arguments: Vec<Ty>,
) -> Ty {
    let names: Vec<Name> = name.split('.').map(Name::new).collect();
    match resolve_qualified(db, from, &Path::parse(package), &names) {
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
        TypeRef::Named {
            segments,
            nullable,
            range,
        } => {
            let nullability = nullability(scope, *nullable);
            let path = Path::new(segments.iter().map(|segment| segment.name));
            let position = range.start as usize;
            let arguments = segments
                .last()
                .map(|segment| lower_arguments(db, scope, &segment.arguments, depth))
                .unwrap_or_default();
            match resolve_type_path(db, scope.from, &path, position) {
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
                _ => Ty::Unknown,
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
            let names: Vec<Name> = classes.iter().map(|segment| segment.name).collect();
            match resolve_qualified(db, scope.from, package, &names) {
                Resolution::Declaration(class) => {
                    class_type(db, scope, class, arguments, nullability, depth)
                }
                _ => Ty::Unknown,
            }
        }
        TypeRef::TypeParameter { name, nullable } => match type_parameter_named(db, scope, *name) {
            Some(parameter) => Ty::TypeParameter {
                parameter,
                nullability: nullability(scope, *nullable),
            },
            None => Ty::Unknown,
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
                Some(kotlin) => builtin(db, scope.from, "kotlin", kotlin, Vec::new()),
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
                    Some(array) => builtin(db, scope.from, "kotlin", &array, Vec::new())
                        .with_nullability(nullability(scope, false)),
                    None => Ty::Unknown,
                };
            }
            let element = lower(db, scope, element, depth);
            builtin(db, scope.from, "kotlin", "Array", vec![element])
                .with_nullability(nullability(scope, false))
        }
        TypeRef::Unreadable => Ty::Unknown,
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
    let owner = scope.owner?;
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
    let mut classes = Vec::new();
    let mut current = Some(class.item);
    while let Some(index) = current {
        let item = tree.item(index);
        classes.push(item.name);
        current = item.parent;
    }
    classes.reverse();
    let written = qualified_name(&tree.package, &classes);
    let Some((_, kotlin)) = MAPPED_TYPES.iter().find(|(java, _)| *java == written) else {
        return class;
    };
    let (package, classes) = split_kotlin_name(kotlin);
    match resolve_qualified(db, from, &package, &classes) {
        Resolution::Declaration(mapped) => mapped,
        _ => class,
    }
}

fn qualified_name(package: &Path, classes: &[Name]) -> String {
    let mut name = package.to_string();
    for class in classes {
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(class.as_str());
    }
    name
}

/// `kotlin.collections.MutableMap.MutableEntry` → package and classes, by
/// the convention that classes start with a capital letter.
fn split_kotlin_name(name: &str) -> (Path, Vec<Name>) {
    let segments: Vec<&str> = name.split('.').collect();
    let first_class = segments
        .iter()
        .position(|segment| segment.starts_with(|first: char| first.is_uppercase()))
        .unwrap_or(segments.len());
    (
        Path::new(
            segments[..first_class]
                .iter()
                .map(|segment| Name::new(segment)),
        ),
        segments[first_class..]
            .iter()
            .map(|segment| Name::new(segment))
            .collect(),
    )
}
