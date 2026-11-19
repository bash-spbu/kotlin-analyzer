//! Overriding as the JVM sees it: which supertype members a member
//! overrides (same name, shape and erased parameter types), which members
//! a static hides, which abstract members a class leaves unimplemented.

use hir_def::item_tree::{ItemKind, Modifiers, Signature};
use hir_def::{ItemLocation, item_tree};
use intern::Name;

use super::{Member, Types, class_substitution, is_java};
use crate::signature::Callable;
use crate::ty::{ClassType, Nullability, Substitution, Ty, TypeParameterId};

/// A class's qualified name, hashed: what the JVM tells classes apart by.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct QualifiedName(u64);

/// A type as overriding compares parameters: by class, as the JVM erases
/// it — by qualified name (two jars may hold one class: a servlet API and
/// a server's copy of it). A function's own type parameter counts by its
/// position (`<T> f(T)` overrides `<U> f(U)`); a class's, as itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ErasedType {
    Class(QualifiedName),
    /// A non-null `Int`, `Long`, …: the JVM's `int`, `long`, which a boxed
    /// `Long` (nullable, a Java `Long`, a type argument from Java) isn't —
    /// AssertJ's `isGreaterThan(long)` beside `isGreaterThan(ACTUAL)`.
    Primitive(ItemLocation),
    OwnTypeParameter(u32),
    TypeParameter(TypeParameterId),
    Function {
        parameters: usize,
    },
}

impl ErasedType {
    /// `None` for a type not known well enough to compare.
    fn of(ty: &Ty, function: ItemLocation) -> Option<ErasedType> {
        match ty {
            // Classes are erased by their qualified names (see `erased_signature`).
            Ty::Class(_) => None,
            Ty::TypeParameter { parameter, .. } if parameter.owner == function => {
                Some(ErasedType::OwnTypeParameter(parameter.index))
            }
            Ty::TypeParameter { parameter, .. } => Some(ErasedType::TypeParameter(*parameter)),
            Ty::Function(function_type) => Some(ErasedType::Function {
                parameters: function_type.parameters.len()
                    + usize::from(function_type.receiver.is_some()),
            }),
            Ty::Nothing { .. } | Ty::Unknown => None,
        }
    }
}

impl Types<'_> {
    /// The members of `member`'s class's supertypes that `member`
    /// overrides: same name, shape and parameter types, compared in the
    /// class's own type parameters. Nearest first.
    pub fn overridden(&self, member: ItemLocation) -> Vec<ItemLocation> {
        let tree = item_tree(self.db, member.container);
        let item = tree.item(member.item);
        let (Some(class), Some(shape)) =
            (item.parent, MemberShape::of(&item.kind, &item.signature))
        else {
            return Vec::new();
        };
        let class = ItemLocation {
            container: member.container,
            item: class,
        };
        let self_type = self.self_type(class);
        let own = Member {
            location: member,
            substitution: class_substitution(&self_type),
        };
        let mut found = Vec::new();
        for supertype in self.all_supertypes(&self_type).into_iter().skip(1) {
            let super_tree = item_tree(self.db, supertype.class.container);
            for candidate in super_tree.named(Some(supertype.class.item), item.name) {
                let candidate_item = super_tree.item(*candidate);
                let same_shape =
                    MemberShape::of(&candidate_item.kind, &candidate_item.signature) == Some(shape);
                let location = ItemLocation {
                    container: supertype.class.container,
                    item: *candidate,
                };
                let inherited = Member {
                    location,
                    substitution: class_substitution(&supertype),
                };
                if same_shape && self.same_signature(&own, &inherited) {
                    found.push(location);
                }
            }
        }
        found
    }

    /// Whether `nearer` overrides `farther` (both found on one receiver, of
    /// one shape): the same erased receiver and parameter types. When a
    /// type isn't known, they count as different — two candidates leave a
    /// call ambiguous, where one wrongly hidden would make it a mismatch.
    /// Whether every type of `member`'s parameters and receiver is known:
    /// only then does [`Types::overridden`] find all it overrides.
    pub fn signature_known(&self, member: ItemLocation) -> bool {
        let tree = item_tree(self.db, member.container);
        let Some(parent) = tree.item(member.item).parent else {
            return false;
        };
        let class = ItemLocation {
            container: member.container,
            item: parent,
        };
        let own = Member {
            location: member,
            substitution: class_substitution(&self.self_type(class)),
        };
        self.erased_signature(&own).is_some()
    }

    /// Whether a supertype of `member`'s class has a member of its name
    /// and kind whose signature has a type not known: it may be the one
    /// `member` overrides.
    pub fn may_override_unknown(&self, member: ItemLocation) -> bool {
        let tree = item_tree(self.db, member.container);
        let item = tree.item(member.item);
        let (Some(class), Some(shape)) =
            (item.parent, MemberShape::of(&item.kind, &item.signature))
        else {
            return false;
        };
        let class = ItemLocation {
            container: member.container,
            item: class,
        };
        let self_type = self.self_type(class);
        self.all_supertypes(&self_type)
            .into_iter()
            .skip(1)
            .any(|supertype| {
                let super_tree = item_tree(self.db, supertype.class.container);
                super_tree
                    .named(Some(supertype.class.item), item.name)
                    .iter()
                    .any(|candidate| {
                        let candidate_item = super_tree.item(*candidate);
                        let inherited = Member {
                            location: ItemLocation {
                                container: supertype.class.container,
                                item: *candidate,
                            },
                            substitution: class_substitution(&supertype),
                        };
                        MemberShape::of(&candidate_item.kind, &candidate_item.signature)
                            == Some(shape)
                            && self.erased_signature(&inherited).is_none()
                    })
            })
    }

    /// The abstract members `class` inherits (or declares) that no
    /// member of its hierarchy implements: none of the same name, kind
    /// and signature, as seen from `class`, isn't abstract. A Java method
    /// without parameters counts as implemented by a property of its name
    /// or its getter's (`size()`, `getName()`: Kotlin's `size`, `name`).
    pub fn unimplemented_members(&self, class: ItemLocation) -> Vec<ItemLocation> {
        let hierarchy = self.all_supertypes(&self.self_type(class));
        // Most classes inherit nothing abstract: only the names of what
        // they do are looked at further (and the Java getters that may
        // implement a property).
        let abstract_names = abstract_names(self, &hierarchy);
        if abstract_names.is_empty() {
            return Vec::new();
        }
        let declared = self.members_concerning(hierarchy, &abstract_names);
        let concrete: Vec<&Declared> = declared.iter().filter(|found| !found.is_abstract).collect();
        declared
            .iter()
            .filter(|found| found.is_abstract)
            .filter(|abstract_member| {
                let implemented = self.implemented_by_member(abstract_member, &concrete)
                    || self.implemented_in_other_terms(abstract_member, &concrete);
                !implemented
            })
            .map(|found| found.member.location)
            .collect()
    }

    /// The non-static members of `hierarchy` named as one of
    /// `abstract_names` or as a Java getter of one.
    fn members_concerning(
        &self,
        hierarchy: Vec<ClassType>,
        abstract_names: &[Name],
    ) -> Vec<Declared> {
        let mut declared: Vec<Declared> = Vec::new();
        for class_type in hierarchy {
            let tree = item_tree(self.db, class_type.class.container);
            let substitution = class_substitution(&class_type);
            for child in tree.item(class_type.class.item).children.iter() {
                let item = tree.item(*child);
                let concerned = concerns_abstract(item.name, abstract_names);
                if item.modifiers.contains(Modifiers::STATIC) || !concerned {
                    continue;
                }
                let Some(shape) = MemberShape::of(&item.kind, &item.signature) else {
                    continue;
                };
                declared.push(Declared {
                    member: Member {
                        location: ItemLocation {
                            container: class_type.class.container,
                            item: *child,
                        },
                        substitution: substitution.clone(),
                    },
                    name: item.name,
                    shape,
                    is_abstract: item.modifiers.contains(Modifiers::ABSTRACT),
                });
            }
        }
        declared
    }

    /// Whether a concrete member of the same name, shape and signature
    /// implements `abstract_member` — a signature with a type not known
    /// may be the same.
    fn implemented_by_member(&self, abstract_member: &Declared, concrete: &[&Declared]) -> bool {
        let may_implement = |candidate: &Declared| {
            let unknown = self.erased_signature(&candidate.member).is_none()
                || self.erased_signature(&abstract_member.member).is_none();
            unknown || self.same_signature(&candidate.member, &abstract_member.member)
        };
        concrete.iter().any(|candidate| {
            candidate.name == abstract_member.name
                && candidate.shape == abstract_member.shape
                && may_implement(candidate)
        })
    }

    /// Whether `abstract_member` is implemented across Kotlin and Java:
    /// a Kotlin property by a Java getter (`val savedStateRegistry`,
    /// `getSavedStateRegistry()`), a Java getter by a Kotlin property, a
    /// built-in collection's member by a Java collection class in its own
    /// terms (`remove(Object)` for `MutableMap.remove(K)`).
    fn implemented_in_other_terms(
        &self,
        abstract_member: &Declared,
        concrete: &[&Declared],
    ) -> bool {
        let java_getter = |declared: &Declared| {
            declared.shape == JAVA_GETTER && is_java(self.db, declared.member.location)
        };
        let property = MemberShape::Property { extension: false };
        let property_by_getter = abstract_member.shape == property
            && concrete.iter().any(|candidate| {
                java_getter(candidate)
                    && self
                        .java_property_name(candidate.member.location)
                        .as_deref()
                        == Some(abstract_member.name.as_str())
            });
        let built_in_collection = item_tree(self.db, abstract_member.member.location.container)
            .package
            .to_string()
            == "kotlin.collections";
        let getter_by_property = java_getter(abstract_member) && {
            let getter_name = self.java_property_name(abstract_member.member.location);
            concrete.iter().any(|candidate| {
                candidate.shape == property
                    && (candidate.name == abstract_member.name
                        || getter_name.as_deref() == Some(candidate.name.as_str()))
            })
        };
        property_by_getter || built_in_collection || getter_by_property
    }

    /// `class`'s qualified name, as the JVM tells classes apart.
    fn qualified_name(&self, class: ItemLocation) -> QualifiedName {
        if let Some(known) = self.qualified_names.borrow().get(&class) {
            return *known;
        }
        let tree = item_tree(self.db, class.container);
        let mut hasher = rustc_hash::FxHasher::default();
        std::hash::Hash::hash(&tree.package, &mut hasher);
        let mut current = Some(class.item);
        while let Some(item) = current {
            let declared = tree.item(item);
            std::hash::Hash::hash(declared.name.as_str(), &mut hasher);
            current = declared.parent;
        }
        let name = QualifiedName(std::hash::Hasher::finish(&hasher));
        self.qualified_names.borrow_mut().insert(class, name);
        name
    }

    /// An `expect` or `actual` class.
    fn is_multiplatform_class(&self, class: ItemLocation) -> bool {
        let modifiers = item_tree(self.db, class.container)
            .item(class.item)
            .modifiers;
        modifiers.contains(Modifiers::EXPECT) || modifiers.contains(Modifiers::ACTUAL)
    }

    /// Whether `class` is one the JVM has a primitive for (`Int`, `Long`,
    /// `Boolean`, `Char`, …).
    fn is_jvm_primitive(&self, class: ItemLocation) -> bool {
        const PRIMITIVES: [&str; 8] = [
            "Int", "Long", "Short", "Byte", "Double", "Float", "Boolean", "Char",
        ];
        PRIMITIVES.iter().any(
            |name| matches!(self.builtin(name), Ty::Class(primitive) if primitive.class == class),
        )
    }

    pub(super) fn same_signature(&self, nearer: &Member, farther: &Member) -> bool {
        match (
            self.erased_signature(nearer),
            self.erased_signature(farther),
        ) {
            (Some(nearer), Some(farther)) => nearer == farther,
            _ => false,
        }
    }

    /// A member's receiver and parameter types as the receiver's class sees
    /// them, erased; `None` when one isn't known.
    fn erased_signature(&self, member: &Member) -> Option<Vec<ErasedType>> {
        let location = member.location;
        let erase = |ty: &Ty| {
            let ty = ty.substitute(&member.substitution);
            let primitive = match &ty {
                Ty::Class(class) if class.nullability == Nullability::NotNull => {
                    self.is_jvm_primitive(class.class)
                }
                _ => false,
            };
            match (primitive, &ty) {
                (true, Ty::Class(class)) => Some(ErasedType::Primitive(class.class)),
                // Common code sees an `expect` class, a platform its `actual`
                // — maybe a type alias to another class: not compared.
                (false, Ty::Class(class)) if self.is_multiplatform_class(class.class) => None,
                // `MutableList` and `List` are both the JVM's `java.util.List`.
                (false, Ty::Class(class)) => Some(ErasedType::Class(
                    self.qualified_name(self.read_only_collection(class).unwrap_or(class.class)),
                )),
                _ => ErasedType::of(&ty, location),
            }
        };
        let tree = item_tree(self.db, location.container);
        if matches!(tree.item(location.item).kind, ItemKind::Property { .. }) {
            return self.property_receiver(location).iter().map(erase).collect();
        }
        let signature = self.signature(Callable::Function(location))?;
        signature
            .receiver
            .iter()
            .chain(signature.parameters.iter().map(|parameter| &parameter.ty))
            .map(erase)
            .collect()
    }

    /// Whether the nearer class's static `nearer` hides its superclass's
    /// `farther`: a field hides a field of its name, a method one with the
    /// same parameter types.
    pub(super) fn hides_static(&self, nearer: ItemLocation, farther: ItemLocation) -> bool {
        let kind_of = |location: ItemLocation| {
            item_tree(self.db, location.container)
                .item(location.item)
                .kind
        };
        match (kind_of(nearer), kind_of(farther)) {
            (ItemKind::Property { .. }, ItemKind::Property { .. }) => true,
            (ItemKind::Function, ItemKind::Function) => {
                let unbound = |location| Member {
                    location,
                    substitution: Substitution::default(),
                };
                self.same_signature(&unbound(nearer), &unbound(farther))
            }
            _ => false,
        }
    }
}

/// A member of a class's hierarchy, as `unimplemented_members` weighs it.
struct Declared {
    member: Member,
    name: Name,
    shape: MemberShape,
    is_abstract: bool,
}

/// The shape of a Java getter: a function without parameters.
const JAVA_GETTER: MemberShape = MemberShape::Function {
    parameters: 0,
    extension: false,
};

/// The names of the abstract non-static members `hierarchy` declares.
fn abstract_names(types: &Types<'_>, hierarchy: &[ClassType]) -> Vec<Name> {
    let mut names: Vec<Name> = Vec::new();
    for class_type in hierarchy {
        let tree = item_tree(types.db, class_type.class.container);
        for child in tree.item(class_type.class.item).children.iter() {
            let item = tree.item(*child);
            let is_abstract = item.modifiers.contains(Modifiers::ABSTRACT)
                && !item.modifiers.contains(Modifiers::STATIC);
            if is_abstract && !names.contains(&item.name) {
                names.push(item.name);
            }
        }
    }
    names
}

/// Whether a member named `name` may implement one of `abstract_names`:
/// it is one, or a Java getter of one (`getName`, `isEmpty`).
fn concerns_abstract(name: Name, abstract_names: &[Name]) -> bool {
    abstract_names.contains(&name)
        || abstract_names.iter().any(|wanted| {
            let getter = name.as_str();
            let property = wanted.as_str();
            getter.len() > 3
                && (getter.starts_with("get") || getter.starts_with("is"))
                && getter.to_lowercase().ends_with(&property.to_lowercase())
        })
}

/// What makes two members the same member for overriding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MemberShape {
    Function { parameters: usize, extension: bool },
    Property { extension: bool },
}

impl MemberShape {
    pub(super) fn of(kind: &ItemKind, signature: &Signature) -> Option<MemberShape> {
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
