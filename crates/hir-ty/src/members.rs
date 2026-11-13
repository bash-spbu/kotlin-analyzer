//! Members of a type: declared and inherited, seen through the type's
//! arguments (`List<String>.get` returns `String`).

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, Signature};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::Name;
use rustc_hash::FxHashSet;

use crate::lower::{TypeScope, builtin, lower_supertype, lower_type};
use crate::ty::{ClassType, Substitution, Ty, TypeParameterId};

/// How many supertypes a type's members are looked up in, at most.
const MAX_SUPERTYPES: usize = 64;

/// A member found on a type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub location: ItemLocation,
    /// The declaring class's type parameters as the receiver binds them.
    pub substitution: Substitution,
}

/// `class_type`'s class's type parameters bound to its arguments.
pub fn class_substitution(class_type: &ClassType) -> Substitution {
    let mut substitution = Substitution::default();
    for (index, argument) in class_type.arguments.iter().enumerate() {
        substitution.insert(
            TypeParameterId {
                owner: class_type.class,
                index: index as u32,
            },
            argument.clone(),
        );
    }
    substitution
}

/// The direct supertypes of `class_type`, in terms of its arguments —
/// `ArrayList<String>` → `AbstractList<String>`, `List<String>`, … — with
/// `Any` (and `Enum<E>` for a Kotlin enum) implied.
pub fn direct_supertypes(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class_type: &ClassType,
) -> Vec<ClassType> {
    let class = class_type.class;
    let tree = item_tree(db, class.container);
    let item = tree.item(class.item);
    let Signature::Class { supertypes, .. } = &item.signature else {
        return Vec::new();
    };
    let substitution = class_substitution(class_type);
    let mut direct: Vec<ClassType> = supertypes
        .iter()
        .filter_map(|supertype| lower_supertype(db, from, class, supertype))
        .filter_map(|supertype| {
            Ty::Class(supertype)
                .substitute(&substitution)
                .class()
                .cloned()
        })
        .collect();
    let is_workspace_enum = matches!(item.kind, ItemKind::Class(ClassKind::Enum))
        && matches!(class.container, ItemContainer::File(_));
    if is_workspace_enum {
        let self_type = Ty::Class(ClassType {
            class,
            arguments: Box::new([]),
            nullability: crate::Nullability::NotNull,
        });
        if let Ty::Class(enum_type) = builtin(db, from, "kotlin", "Enum", vec![self_type]) {
            direct.push(enum_type);
        }
    }
    if let Ty::Class(any) = builtin(db, from, "kotlin", "Any", Vec::new())
        && any.class != class
    {
        direct.push(any);
    }
    direct
}

/// `class_type` and all its supertypes, nearest first.
pub fn all_supertypes(
    db: &dyn SourceDatabase,
    from: SourceFile,
    class_type: &ClassType,
) -> Vec<ClassType> {
    let mut seen = FxHashSet::default();
    seen.insert(class_type.class);
    let mut all = vec![class_type.clone()];
    let mut next = 0;
    while next < all.len() && all.len() < MAX_SUPERTYPES {
        let current = all[next].clone();
        next += 1;
        for supertype in direct_supertypes(db, from, &current) {
            if seen.insert(supertype.class) {
                all.push(supertype);
            }
        }
    }
    all
}

/// The class types whose members a value of type `ty` has: its class, or a
/// type parameter's bounds (`Any` when it declares none).
pub fn member_holders(db: &dyn SourceDatabase, from: SourceFile, ty: &Ty) -> Vec<ClassType> {
    match ty {
        Ty::Class(class) => vec![class.clone()],
        Ty::TypeParameter { parameter, .. } => {
            let bounds: Vec<ClassType> = type_parameter_bounds(db, from, *parameter)
                .iter()
                .filter_map(|bound| bound.class().cloned())
                .collect();
            if bounds.is_empty() {
                builtin(db, from, "kotlin", "Any", Vec::new())
                    .class()
                    .cloned()
                    .into_iter()
                    .collect()
            } else {
                bounds
            }
        }
        Ty::Function(_) | Ty::Nothing { .. } | Ty::Unknown => Vec::new(),
    }
}

/// The declared upper bounds of a type parameter.
pub fn type_parameter_bounds(
    db: &dyn SourceDatabase,
    from: SourceFile,
    parameter: TypeParameterId,
) -> Vec<Ty> {
    let tree = item_tree(db, parameter.owner.container);
    let Some(declared) = tree
        .item(parameter.owner.item)
        .type_parameters
        .get(parameter.index as usize)
    else {
        return Vec::new();
    };
    let scope = TypeScope::of_declaration(db, from, parameter.owner);
    declared
        .bounds
        .iter()
        .map(|bound| lower_type(db, scope, bound))
        .collect()
}

/// The functions and properties named `name` a value of type `receiver`
/// has, the most derived first; an inherited member a nearer class
/// overrides (same kind, same number of parameters) is left out.
pub fn members_named(
    db: &dyn SourceDatabase,
    from: SourceFile,
    receiver: &Ty,
    name: Name,
) -> Vec<Member> {
    let mut found: Vec<(Member, MemberShape)> = Vec::new();
    for holder in member_holders(db, from, receiver) {
        for class_type in all_supertypes(db, from, &holder) {
            let tree = item_tree(db, class_type.class.container);
            let substitution = class_substitution(&class_type);
            for child in tree.item(class_type.class.item).children.iter() {
                let item = tree.item(*child);
                if item.name != name || item.modifiers.contains(Modifiers::STATIC) {
                    continue;
                }
                let Some(shape) = MemberShape::of(&item.kind, &item.signature) else {
                    continue;
                };
                if found.iter().any(|(_, known)| *known == shape) {
                    continue;
                }
                let location = ItemLocation {
                    container: class_type.class.container,
                    item: *child,
                };
                found.push((
                    Member {
                        location,
                        substitution: substitution.clone(),
                    },
                    shape,
                ));
            }
        }
    }
    found.into_iter().map(|(member, _)| member).collect()
}

/// The getter a Java class has for the property `name` Kotlin lets you
/// write (`user.name` for `getName()`, `user.active` for `isActive()`).
pub fn java_getters(
    db: &dyn SourceDatabase,
    from: SourceFile,
    receiver: &Ty,
    name: Name,
) -> Vec<Member> {
    let spelled = name.as_str();
    let Some(first) = spelled.chars().next() else {
        return Vec::new();
    };
    let capitalized: String = first
        .to_uppercase()
        .chain(spelled.chars().skip(1))
        .collect();
    let mut getters = Vec::new();
    for prefix in ["get", "is"] {
        let getter = Name::new(&format!("{prefix}{capitalized}"));
        getters.extend(
            members_named(db, from, receiver, getter)
                .into_iter()
                .filter(|member| is_java_getter(db, member.location)),
        );
    }
    getters
}

fn is_java_getter(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    let tree = item_tree(db, location.container);
    let item = tree.item(location.item);
    let is_java = match location.container {
        ItemContainer::File(file) => file.language(db) == syntax::Language::Java,
        ItemContainer::Library(_) => item.modifiers.contains(Modifiers::PLATFORM),
    };
    let takes_nothing = matches!(
        &item.signature,
        Signature::Function { parameters, return_type: Some(_), .. } if parameters.is_empty()
    );
    is_java && takes_nothing
}

/// What makes two members the same member for overriding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MemberShape {
    Function { parameters: usize, extension: bool },
    Property { extension: bool },
}

impl MemberShape {
    fn of(kind: &ItemKind, signature: &Signature) -> Option<MemberShape> {
        match (kind, signature) {
            (
                ItemKind::Function,
                Signature::Function {
                    receiver,
                    parameters,
                    ..
                },
            ) => Some(MemberShape::Function {
                parameters: parameters.len(),
                extension: receiver.is_some(),
            }),
            (ItemKind::Property { .. }, Signature::Property { receiver, .. }) => {
                Some(MemberShape::Property {
                    extension: receiver.is_some(),
                })
            }
            _ => None,
        }
    }
}
