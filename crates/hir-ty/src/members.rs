//! Members of a type: declared and inherited, seen through the type's
//! arguments (`List<String>.get` returns `String`).

use std::cell::RefCell;
use std::rc::Rc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, Signature};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::Name;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::lower::{TypeScope, builtin, lower_supertype, lower_type};
use crate::ty::{ClassType, Nullability, Substitution, Ty, TypeParameterId};

/// How many supertypes a type's members are looked up in, at most.
const MAX_SUPERTYPES: usize = 64;

/// A member found on a type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub location: ItemLocation,
    /// The declaring class's type parameters as the receiver binds them.
    pub substitution: Substitution,
}

/// Type questions asked from one file — whose classpath library types
/// resolve through — remembering class hierarchies between questions.
pub struct Types<'db> {
    pub db: &'db dyn SourceDatabase,
    pub from: SourceFile,
    /// Each class's supertypes in terms of its own type parameters.
    hierarchies: RefCell<FxHashMap<ItemLocation, Rc<[ClassType]>>>,
    complete_hierarchies: RefCell<FxHashMap<ItemLocation, bool>>,
    pub(crate) signatures: RefCell<
        FxHashMap<crate::signature::Callable, Option<Rc<crate::signature::CallableSignature>>>,
    >,
}

impl<'db> Types<'db> {
    pub fn new(db: &'db dyn SourceDatabase, from: SourceFile) -> Types<'db> {
        Types {
            db,
            from,
            hierarchies: RefCell::default(),
            complete_hierarchies: RefCell::default(),
            signatures: RefCell::default(),
        }
    }

    /// `kotlin.<name>` (`Any`, `String`, `Unit`, …) without arguments.
    pub fn builtin(&self, name: &str) -> Ty {
        builtin(self.db, self.from, "kotlin", name, Vec::new())
    }

    /// A class's type with its own type parameters as arguments — the type
    /// of `this` inside it.
    pub fn self_type(&self, class: ItemLocation) -> ClassType {
        let tree = item_tree(self.db, class.container);
        let count = tree.item(class.item).type_parameters.len();
        ClassType {
            class,
            arguments: (0..count)
                .map(|index| Ty::TypeParameter {
                    parameter: TypeParameterId {
                        owner: class,
                        index: index as u32,
                    },
                    nullability: Nullability::NotNull,
                })
                .collect(),
            nullability: Nullability::NotNull,
        }
    }

    /// The direct supertypes of `class_type`, in terms of its arguments —
    /// `ArrayList<String>` → `AbstractList<String>`, `List<String>`, … —
    /// with `Any` (and `Enum<E>` for a Kotlin enum) implied.
    pub fn direct_supertypes(&self, class_type: &ClassType) -> Vec<ClassType> {
        let class = class_type.class;
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        let Signature::Class { supertypes, .. } = &item.signature else {
            return Vec::new();
        };
        let substitution = class_substitution(class_type);
        let mut direct: Vec<ClassType> = supertypes
            .iter()
            .filter_map(|supertype| lower_supertype(self.db, self.from, class, supertype))
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
                nullability: Nullability::NotNull,
            });
            if let Ty::Class(enum_type) =
                builtin(self.db, self.from, "kotlin", "Enum", vec![self_type])
            {
                direct.push(enum_type);
            }
        }
        if let Ty::Class(any) = self.builtin("Any")
            && any.class != class
        {
            direct.push(any);
        }
        direct
    }

    /// `class_type` and all its supertypes, nearest first.
    pub fn all_supertypes(&self, class_type: &ClassType) -> Vec<ClassType> {
        let hierarchy = self.hierarchy(class_type.class);
        let substitution = class_substitution(class_type);
        let mut all = Vec::with_capacity(hierarchy.len() + 1);
        all.push(class_type.clone());
        all.extend(hierarchy.iter().filter_map(|supertype| {
            Ty::Class(supertype.clone())
                .substitute(&substitution)
                .class()
                .cloned()
        }));
        all
    }

    /// The supertypes of `class` (not itself) in terms of its own type
    /// parameters, breadth first.
    fn hierarchy(&self, class: ItemLocation) -> Rc<[ClassType]> {
        if let Some(known) = self.hierarchies.borrow().get(&class) {
            return known.clone();
        }
        let mut seen = FxHashSet::default();
        seen.insert(class);
        let mut all = vec![self.self_type(class)];
        let mut next = 0;
        while next < all.len() && all.len() < MAX_SUPERTYPES {
            let current = all[next].clone();
            next += 1;
            for supertype in self.direct_supertypes(&current) {
                if seen.insert(supertype.class) {
                    all.push(supertype);
                }
            }
        }
        let hierarchy: Rc<[ClassType]> = all.into_iter().skip(1).collect();
        self.hierarchies
            .borrow_mut()
            .insert(class, hierarchy.clone());
        hierarchy
    }

    /// Whether every supertype written in `class`'s hierarchy resolves —
    /// only then are its members all known.
    pub fn hierarchy_complete(&self, class: ItemLocation) -> bool {
        if let Some(known) = self.complete_hierarchies.borrow().get(&class) {
            return *known;
        }
        let mut classes = vec![class];
        classes.extend(
            self.hierarchy(class)
                .iter()
                .map(|supertype| supertype.class),
        );
        let complete = classes.iter().all(|class| {
            let tree = item_tree(self.db, class.container);
            let Signature::Class { supertypes, .. } = &tree.item(class.item).signature else {
                return true;
            };
            supertypes
                .iter()
                .all(|supertype| lower_supertype(self.db, self.from, *class, supertype).is_some())
        });
        self.complete_hierarchies
            .borrow_mut()
            .insert(class, complete);
        complete
    }

    /// `ty`'s supertype instance of `class`: `ArrayList<String>` seen as
    /// `List` is `List<String>`.
    pub fn as_supertype(&self, ty: &Ty, class: ItemLocation) -> Option<ClassType> {
        self.member_holders(ty)
            .iter()
            .flat_map(|holder| self.all_supertypes(holder))
            .find(|supertype| supertype.class == class)
    }

    /// The class types whose members a value of type `ty` has: its class,
    /// or a type parameter's bounds (`Any` when it declares none).
    pub fn member_holders(&self, ty: &Ty) -> Vec<ClassType> {
        match ty {
            Ty::Class(class) => vec![class.clone()],
            Ty::TypeParameter { parameter, .. } => {
                let bounds: Vec<ClassType> = self
                    .type_parameter_bounds(*parameter)
                    .iter()
                    .filter_map(|bound| bound.class().cloned())
                    .collect();
                if bounds.is_empty() {
                    self.builtin("Any").class().cloned().into_iter().collect()
                } else {
                    bounds
                }
            }
            Ty::Function(_) | Ty::Nothing { .. } | Ty::Unknown => Vec::new(),
        }
    }

    /// The declared upper bounds of a type parameter.
    pub fn type_parameter_bounds(&self, parameter: TypeParameterId) -> Vec<Ty> {
        let tree = item_tree(self.db, parameter.owner.container);
        let Some(declared) = tree
            .item(parameter.owner.item)
            .type_parameters
            .get(parameter.index as usize)
        else {
            return Vec::new();
        };
        let scope = TypeScope::of_declaration(self.db, self.from, parameter.owner);
        declared
            .bounds
            .iter()
            .map(|bound| lower_type(self.db, scope, bound))
            .collect()
    }

    /// The functions and properties named `name` a value of type
    /// `receiver` has, the most derived first; an inherited member a nearer
    /// class overrides (same kind, same number of parameters) is left out.
    pub fn members_named(&self, receiver: &Ty, name: Name) -> Vec<Member> {
        let mut found: Vec<(Member, MemberShape)> = Vec::new();
        for holder in self.member_holders(receiver) {
            for class_type in self.all_supertypes(&holder) {
                let tree = item_tree(self.db, class_type.class.container);
                let mut substitution = None;
                // Overloads of one class all stay; only nearer classes'
                // members hide (override) farther ones.
                let from_nearer_classes = found.len();
                for child in tree.item(class_type.class.item).children.iter() {
                    let item = tree.item(*child);
                    if item.name != name || item.modifiers.contains(Modifiers::STATIC) {
                        continue;
                    }
                    let Some(shape) = MemberShape::of(&item.kind, &item.signature) else {
                        continue;
                    };
                    if found[..from_nearer_classes]
                        .iter()
                        .any(|(_, known)| *known == shape)
                    {
                        continue;
                    }
                    let substitution =
                        substitution.get_or_insert_with(|| class_substitution(&class_type));
                    found.push((
                        Member {
                            location: ItemLocation {
                                container: class_type.class.container,
                                item: *child,
                            },
                            substitution: substitution.clone(),
                        },
                        shape,
                    ));
                }
            }
        }
        found.into_iter().map(|(member, _)| member).collect()
    }

    /// Which parameters of the member function `member` have a default
    /// value declared by a function it overrides (same name, same number
    /// of parameters, in a supertype of `receiver`'s).
    pub fn inherited_defaults(&self, receiver: &Ty, member: ItemLocation) -> Vec<bool> {
        let tree = item_tree(self.db, member.container);
        let item = tree.item(member.item);
        let Signature::Function { parameters, .. } = &item.signature else {
            return Vec::new();
        };
        let mut defaults: Vec<bool> = parameters
            .iter()
            .map(|parameter| parameter.has_default)
            .collect();
        for holder in self.member_holders(receiver) {
            for class_type in self.all_supertypes(&holder) {
                let class_tree = item_tree(self.db, class_type.class.container);
                for child in class_tree.item(class_type.class.item).children.iter() {
                    let candidate = class_tree.item(*child);
                    let Signature::Function {
                        parameters: overridden,
                        ..
                    } = &candidate.signature
                    else {
                        continue;
                    };
                    if candidate.name != item.name || overridden.len() != defaults.len() {
                        continue;
                    }
                    for (has_default, parameter) in defaults.iter_mut().zip(overridden.iter()) {
                        *has_default |= parameter.has_default;
                    }
                }
            }
        }
        defaults
    }

    /// The getter a Java class has for the property `name` Kotlin lets you
    /// write (`user.name` for `getName()`, `user.active` for `isActive()`).
    pub fn java_getters(&self, receiver: &Ty, name: Name) -> Vec<Member> {
        let spelled = name.as_str();
        let Some(first) = spelled.chars().next() else {
            return Vec::new();
        };
        if !first.is_lowercase() {
            return Vec::new();
        }
        let capitalized: String = first
            .to_uppercase()
            .chain(spelled.chars().skip(1))
            .collect();
        let mut getters = Vec::new();
        for prefix in ["get", "is"] {
            let getter = Name::new(&format!("{prefix}{capitalized}"));
            getters.extend(
                self.members_named(receiver, getter)
                    .into_iter()
                    .filter(|member| is_java_getter(self.db, member.location)),
            );
        }
        getters
    }

    /// What a class's name means where a value is expected: an object's
    /// own instance, or a class's companion object (`Money.ZERO`).
    pub fn object_instance(&self, class: ItemLocation) -> Option<ClassType> {
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        match item.kind {
            ItemKind::Class(ClassKind::Object | ClassKind::CompanionObject) => {
                Some(self.self_type(class))
            }
            ItemKind::Class(_) => self
                .companion(class)
                .map(|companion| self.self_type(companion)),
            _ => None,
        }
    }

    /// A class's companion object.
    pub fn companion(&self, class: ItemLocation) -> Option<ItemLocation> {
        let tree = item_tree(self.db, class.container);
        tree.item(class.item)
            .children
            .iter()
            .find(|child| tree.item(**child).kind == ItemKind::Class(ClassKind::CompanionObject))
            .map(|child| ItemLocation {
                container: class.container,
                item: *child,
            })
    }

    /// What `Class.name` can mean besides the companion's members: a
    /// nested classifier, an enum entry, or a Java static member.
    pub fn static_members_named(&self, class: ItemLocation, name: Name) -> Vec<ItemLocation> {
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        let mut found: Vec<ItemLocation> = item
            .children
            .iter()
            .filter(|child| {
                let child = tree.item(**child);
                let is_static = child.modifiers.contains(Modifiers::STATIC)
                    || child.kind == ItemKind::EnumEntry
                    || child.kind.is_classifier();
                child.name == name && is_static
            })
            .map(|child| ItemLocation {
                container: class.container,
                item: *child,
            })
            .collect();
        // Java statics are inherited: `ArrayList.copyOf` is not a thing, but
        // `MyHandler.INSTANCE` of a superclass is.
        if found.is_empty() && item.modifiers.contains(Modifiers::PLATFORM) {
            for supertype in self.hierarchy(class).iter() {
                let tree = item_tree(self.db, supertype.class.container);
                found.extend(
                    tree.item(supertype.class.item)
                        .children
                        .iter()
                        .filter(|child| {
                            let child = tree.item(**child);
                            child.name == name && child.modifiers.contains(Modifiers::STATIC)
                        })
                        .map(|child| ItemLocation {
                            container: supertype.class.container,
                            item: *child,
                        }),
                );
                if !found.is_empty() {
                    break;
                }
            }
        }
        found
    }
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

fn is_java_getter(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    let tree = item_tree(db, location.container);
    let item = tree.item(location.item);
    let takes_nothing = matches!(
        &item.signature,
        Signature::Function { parameters, return_type: Some(_), .. } if parameters.is_empty()
    );
    is_java(db, location) && takes_nothing
}

/// Whether a declaration is Java's.
pub fn is_java(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    match location.container {
        ItemContainer::File(file) => file.language(db) == syntax::Language::Java,
        ItemContainer::Library(_) => item_tree(db, location.container)
            .item(location.item)
            .modifiers
            .contains(Modifiers::PLATFORM),
    }
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
