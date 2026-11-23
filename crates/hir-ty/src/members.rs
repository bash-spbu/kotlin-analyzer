//! Members of a type: declared and inherited, seen through the type's
//! arguments (`List<String>.get` returns `String`).

use std::cell::RefCell;
use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile, SourceSet};
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, Signature};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::Name;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::lower::{TypeScope, builtin, lower_supertype, lower_type};
use crate::ty::{ClassType, Nullability, Substitution, Ty, TypeParameterId};

mod overrides;

use overrides::{MemberShape, QualifiedName};

/// How many supertypes a type's members are looked up in, at most.
const MAX_SUPERTYPES: usize = 64;

/// A member found on a type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Member {
    pub location: ItemLocation,
    /// The declaring class's type parameters as the receiver binds them.
    pub substitution: Substitution,
}

/// `class`'s supertypes as the files of `source_set` see them — what
/// [`Types`] would compute for each of them, computed once. Library types
/// resolve through the source set's classpath; the file that stands for
/// it is its first, whose `private` classes could in principle stand in
/// for a library's supertype of the same qualified name.
#[salsa::tracked(returns(clone), lru = 8192)]
pub(crate) fn source_set_hierarchy(
    db: &dyn SourceDatabase,
    source_set: SourceSet,
    class: ItemLocation,
) -> Arc<[ClassType]> {
    match source_set.files(db).first() {
        Some(file) => Types::new(db, *file).compute_hierarchy(class),
        None => Arc::new([]),
    }
}

/// Type questions asked from one file — whose classpath library types
/// resolve through — remembering class hierarchies between questions.
/// What depends only on the file's source set (a hierarchy, a callable's
/// signature) is also shared between its files.
pub struct Types<'db> {
    pub db: &'db dyn SourceDatabase,
    pub from: SourceFile,
    /// Each class's supertypes in terms of its own type parameters.
    hierarchies: RefCell<FxHashMap<ItemLocation, Arc<[ClassType]>>>,
    complete_hierarchies: RefCell<FxHashMap<ItemLocation, bool>>,
    qualified_names: RefCell<FxHashMap<ItemLocation, QualifiedName>>,
    /// What `members_named` found, by receiver and name: an unqualified
    /// call asks it of every receiver for every level.
    members_by_name: RefCell<FxHashMap<(Ty, Name), Vec<Member>>>,
    pub(crate) signatures: RefCell<
        FxHashMap<crate::signature::Callable, Option<Arc<crate::signature::CallableSignature>>>,
    >,
    /// `kotlin.<name>` types by name: asked for on every fit check.
    builtins: RefCell<FxHashMap<Box<str>, Ty>>,
    /// Each type parameter's upper bounds: asked for on every fit check of
    /// a generic call.
    bounds: RefCell<FxHashMap<TypeParameterId, Arc<[Ty]>>>,
    /// A mutable collection class's read-only counterpart.
    pub(crate) read_only_collections: RefCell<FxHashMap<ItemLocation, Option<ItemLocation>>>,
}

impl<'db> Types<'db> {
    pub fn new(db: &'db dyn SourceDatabase, from: SourceFile) -> Types<'db> {
        Types {
            db,
            from,
            hierarchies: RefCell::default(),
            complete_hierarchies: RefCell::default(),
            qualified_names: RefCell::default(),
            members_by_name: RefCell::default(),
            signatures: RefCell::default(),
            builtins: RefCell::default(),
            bounds: RefCell::default(),
            read_only_collections: RefCell::default(),
        }
    }

    /// `kotlin.<name>` (`Any`, `String`, `Unit`, …) without arguments.
    pub fn builtin(&self, name: &str) -> Ty {
        if let Some(known) = self.builtins.borrow().get(name) {
            return known.clone();
        }
        let found = builtin(
            self.db,
            self.from,
            crate::lower::KnownPackage::Kotlin,
            name,
            Vec::new(),
        );
        self.builtins
            .borrow_mut()
            .insert(name.into(), found.clone());
        found
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
            if let Ty::Class(enum_type) = builtin(
                self.db,
                self.from,
                crate::lower::KnownPackage::Kotlin,
                "Enum",
                vec![self_type],
            ) {
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
    pub(crate) fn hierarchy(&self, class: ItemLocation) -> Arc<[ClassType]> {
        if let Some(known) = self.hierarchies.borrow().get(&class) {
            return known.clone();
        }
        let hierarchy = match self.shared_by() {
            Some(source_set) => source_set_hierarchy(self.db, source_set, class),
            None => self.compute_hierarchy(class),
        };
        self.hierarchies
            .borrow_mut()
            .insert(class, hierarchy.clone());
        hierarchy
    }

    /// The source set whose files share what this file computes; none for
    /// a file outside every source set.
    pub(crate) fn shared_by(&self) -> Option<SourceSet> {
        hir_def::def_map::source_set_input(self.db, self.from)
    }

    fn compute_hierarchy(&self, class: ItemLocation) -> Arc<[ClassType]> {
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
        all.into_iter().skip(1).collect()
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
        // Only the supertype found is substituted, not the whole hierarchy.
        self.member_holders(ty).iter().find_map(|holder| {
            if holder.class == class {
                return Some(holder.clone());
            }
            let generic = self
                .hierarchy(holder.class)
                .iter()
                .find(|supertype| supertype.class == class)?
                .clone();
            Ty::Class(generic)
                .substitute(&class_substitution(holder))
                .class()
                .cloned()
        })
    }

    /// The class types whose members a value of type `ty` has: its class,
    /// or a type parameter's bounds (`Any` when it declares none).
    pub fn member_holders(&self, ty: &Ty) -> Vec<ClassType> {
        match ty {
            Ty::Class(class) => vec![self.seen_as_actual(class)],
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
            Ty::Function(_) | Ty::Nothing { .. } | Ty::Unknown | Ty::Error => Vec::new(),
        }
    }

    /// The declared upper bounds of a type parameter (remembered).
    pub fn type_parameter_bounds(&self, parameter: TypeParameterId) -> Arc<[Ty]> {
        if let Some(known) = self.bounds.borrow().get(&parameter) {
            return known.clone();
        }
        let bounds: Arc<[Ty]> = self.declared_bounds(parameter).into();
        self.bounds.borrow_mut().insert(parameter, bounds.clone());
        bounds
    }

    fn declared_bounds(&self, parameter: TypeParameterId) -> Vec<Ty> {
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
        let key = (receiver.clone(), name);
        if let Some(known) = self.members_by_name.borrow().get(&key) {
            return known.clone();
        }
        let found = self.visible_members(receiver, Wanted::Named(name));
        self.members_by_name.borrow_mut().insert(key, found.clone());
        found
    }

    /// The non-static members of `receiver`'s hierarchy whose name `wanted`
    /// takes, the most derived first. Overloads of one class all stay; a
    /// nearer class's member hides (overrides) a farther one of the same
    /// name, shape and parameter types — compared in terms of the
    /// receiver's class's own type parameters, so an argument not inferred
    /// yet (`buildList { add(…) }`) doesn't hide an override.
    fn visible_members(&self, receiver: &Ty, wanted: Wanted) -> Vec<Member> {
        struct Found {
            member: Member,
            name: Name,
            shape: MemberShape,
            generic: Member,
        }
        let mut found: Vec<Found> = Vec::new();
        for holder in self.member_holders(receiver) {
            let generic_supertypes = self.all_supertypes(&self.self_type(holder.class));
            for class_type in self.all_supertypes(&holder) {
                let tree = item_tree(self.db, class_type.class.container);
                let from_nearer_classes = found.len();
                let mut substitutions = None;
                let candidates = match wanted {
                    Wanted::Named(name) => tree.named(Some(class_type.class.item), name),
                    Wanted::All => &tree.item(class_type.class.item).children,
                };
                for child in candidates.iter() {
                    let item = tree.item(*child);
                    let location = ItemLocation {
                        container: class_type.class.container,
                        item: *child,
                    };
                    if item.modifiers.contains(Modifiers::STATIC) || !self.may_see(location, item) {
                        continue;
                    }
                    let Some(shape) = MemberShape::of(&item.kind, &item.signature) else {
                        continue;
                    };
                    let (substitution, generic_substitution) = substitutions
                        .get_or_insert_with(|| {
                            let generic = generic_supertypes
                                .iter()
                                .find(|generic| generic.class == class_type.class)
                                .map(class_substitution)
                                .unwrap_or_default();
                            (class_substitution(&class_type), generic)
                        })
                        .clone();
                    let generic = Member {
                        location,
                        substitution: generic_substitution,
                    };
                    let is_property = matches!(item.kind, ItemKind::Property { .. });
                    let overridden = found[..from_nearer_classes].iter().any(|known| {
                        // A property of a sibling supertype isn't overridden
                        // by the other's: both make an intersection override.
                        known.name == item.name
                            && known.shape == shape
                            && self.same_signature(&known.generic, &generic)
                            && (!is_property
                                || self
                                    .declares_subclass_of(known.member.location, class_type.class))
                    });
                    if !overridden {
                        found.push(Found {
                            member: Member {
                                location,
                                substitution,
                            },
                            name: item.name,
                            shape,
                            generic,
                        });
                    }
                }
            }
        }
        found.into_iter().map(|found| found.member).collect()
    }

    /// Whether `member`'s class is `class` or a subclass of it.
    fn declares_subclass_of(&self, member: ItemLocation, class: ItemLocation) -> bool {
        let tree = item_tree(self.db, member.container);
        let Some(parent) = tree.item(member.item).parent else {
            return false;
        };
        let declaring = ItemLocation {
            container: member.container,
            item: parent,
        };
        declaring == class
            || self
                .hierarchy(declaring)
                .iter()
                .any(|supertype| supertype.class == class)
    }

    /// Whether code in this file may call `member`: a `private` function
    /// only in the file declaring it — never another file's, nor a
    /// library's, so an extension of the name is called instead (Kotlin
    /// narrows it to the class body; within one file that isn't followed).
    /// A private property stays: Java's private field stands in for its
    /// getter's property where that synthesis isn't followed.
    fn may_see(&self, member: ItemLocation, item: &hir_def::item_tree::Item) -> bool {
        let private_function = item.kind == ItemKind::Function
            && item.visibility == hir_def::item_tree::Visibility::Private;
        !private_function || member.container == ItemContainer::File(self.from)
    }

    /// The `inner` classes named `name` of `receiver`'s class and its
    /// supertypes: what `outer.Inner(…)` constructs.
    pub fn inner_classes_named(&self, receiver: &Ty, name: Name) -> Vec<ItemLocation> {
        let mut found = Vec::new();
        for holder in self.member_holders(receiver) {
            for class_type in self.all_supertypes(&holder) {
                let tree = item_tree(self.db, class_type.class.container);
                for child in tree.named(Some(class_type.class.item), name) {
                    let item = tree.item(*child);
                    let is_inner_class =
                        item.kind.is_classifier() && item.modifiers.contains(Modifiers::INNER);
                    if is_inner_class {
                        found.push(ItemLocation {
                            container: class_type.class.container,
                            item: *child,
                        });
                    }
                }
            }
        }
        found
    }

    /// Which parameters of the member function `member` have a default
    /// value declared by a function it overrides (same name, same
    /// parameter types, in a supertype of `receiver`'s).
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
        // Compared in the receiver's class's own type parameters, as
        // overriding is (see `visible_members`).
        let class_types: Vec<ClassType> = self
            .member_holders(receiver)
            .iter()
            .flat_map(|holder| self.all_supertypes(&self.self_type(holder.class)))
            .collect();
        let declaring = class_types.iter().find(|class_type| {
            Some(class_type.class.item) == item.parent
                && class_type.class.container == member.container
        });
        let member = Member {
            location: member,
            substitution: declaring.map(class_substitution).unwrap_or_default(),
        };
        for class_type in &class_types {
            let class_tree = item_tree(self.db, class_type.class.container);
            for child in class_tree.named(Some(class_type.class.item), item.name) {
                let candidate = class_tree.item(*child);
                let Signature::Function {
                    parameters: overridden,
                    ..
                } = &candidate.signature
                else {
                    continue;
                };
                if overridden.len() != defaults.len() {
                    continue;
                }
                let candidate = Member {
                    location: ItemLocation {
                        container: class_type.class.container,
                        item: *child,
                    },
                    substitution: class_substitution(class_type),
                };
                if !self.same_signature(&member, &candidate) {
                    continue;
                }
                for (has_default, parameter) in defaults.iter_mut().zip(overridden.iter()) {
                    *has_default |= parameter.has_default;
                }
            }
        }
        defaults
    }

    /// Every function and property a value of type `receiver` has, the
    /// most derived first; overridden ones left out.
    pub fn all_members(&self, receiver: &Ty) -> Vec<Member> {
        self.visible_members(receiver, Wanted::All)
    }

    /// The property name Kotlin gives a Java getter (`getTitle` → `title`,
    /// `getURL` → `url`, `isOpen` stays), when `member` is one.
    pub fn java_property_name(&self, member: ItemLocation) -> Option<String> {
        if !is_java_getter(self.db, member) {
            return None;
        }
        let tree = item_tree(self.db, member.container);
        getter_property_name(tree.item(member.item).name.as_str())
    }

    /// The getters a Java class has for the property `name` Kotlin lets you
    /// write: `user.name` for `getName()`, `page.url` for `getURL()`,
    /// `user.isActive` for `isActive()`.
    pub fn java_getters(&self, receiver: &Ty, name: Name) -> Vec<Member> {
        let spelled = name.as_str();
        let names_property = |member: &Member| {
            is_java_getter(self.db, member.location)
                && self.java_property_name(member.location).as_deref() == Some(spelled)
        };
        let Some(first) = spelled.chars().next() else {
            return Vec::new();
        };
        // A getter's name is looked up, never interned: one nothing declares
        // isn't interned, and the interner keeps every name for good.
        let mut getter = String::with_capacity(spelled.len() + 3);
        let likely_getter = if getter_property_name(spelled).as_deref() == Some(spelled) {
            Some(name)
        } else {
            getter.push_str("get");
            getter.extend(first.to_uppercase());
            getter.push_str(&spelled[first.len_utf8()..]);
            Name::existing(&getter)
        };
        if let Some(likely_getter) = likely_getter {
            let getters: Vec<Member> = self
                .members_named(receiver, likely_getter)
                .into_iter()
                .filter(names_property)
                .collect();
            if !getters.is_empty() {
                return getters;
            }
        }
        // An acronym's getter: `getURL` for `url`, `getURLName` for `urlName`
        // — the leading lower-case run upper-cased, in part or whole.
        let run = spelled
            .chars()
            .take_while(|char| char.is_ascii_lowercase())
            .count();
        let mut found = Vec::new();
        for upper in 2..=run {
            getter.clear();
            getter.push_str("get");
            getter.extend(
                spelled[..upper]
                    .chars()
                    .map(|char| char.to_ascii_uppercase()),
            );
            getter.push_str(&spelled[upper..]);
            let Some(acronym) = Name::existing(&getter) else {
                continue;
            };
            found.extend(
                self.members_named(receiver, acronym)
                    .into_iter()
                    .filter(names_property),
            );
        }
        found
    }

    /// An `expect` class as the asking file sees it: its `actual` when that
    /// is visible (a type a common declaration gives, used from the JVM),
    /// through a type alias to what the alias names.
    fn seen_as_actual(&self, class: &ClassType) -> ClassType {
        let actual = hir_def::expect_actual::actual_seen_from(self.db, self.from, class.class);
        if actual == class.class {
            return class.clone();
        }
        let tree = item_tree(self.db, actual.container);
        let Signature::TypeAlias { target } = &tree.item(actual.item).signature else {
            return ClassType {
                class: actual,
                ..class.clone()
            };
        };
        let scope = crate::lower::TypeScope::of_declaration(self.db, self.from, actual);
        match crate::lower::lower_type(self.db, scope, target) {
            Ty::Class(expanded) => ClassType {
                nullability: class.nullability,
                ..expanded
            },
            _ => class.clone(),
        }
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
        let mut found: Vec<ItemLocation> = tree
            .named(Some(class.item), name)
            .iter()
            .filter(|child| {
                let child = tree.item(**child);
                child.modifiers.contains(Modifiers::STATIC)
                    || child.kind == ItemKind::EnumEntry
                    || child.kind.is_classifier()
            })
            .map(|child| ItemLocation {
                container: class.container,
                item: *child,
            })
            .collect();
        // Java statics are inherited: `MyHandler.INSTANCE` of a superclass
        // is reachable through the subclass, and so are a superclass's
        // overloads of a static method — unless the nearer class hides one
        // with the same parameter types.
        if !is_java(self.db, class) {
            return found;
        }
        for supertype in self.hierarchy(class).iter() {
            let tree = item_tree(self.db, supertype.class.container);
            let from_nearer_classes = found.len();
            for child in tree.named(Some(supertype.class.item), name) {
                let candidate = tree.item(*child);
                if !candidate.modifiers.contains(Modifiers::STATIC) {
                    continue;
                }
                let location = ItemLocation {
                    container: supertype.class.container,
                    item: *child,
                };
                let hidden = found[..from_nearer_classes]
                    .iter()
                    .any(|nearer| self.hides_static(*nearer, location));
                if !hidden {
                    found.push(location);
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

/// The property name Kotlin gives a Java getter named `getter`:
/// `getTitle` → `title`, `getURL` → `url`, `getURLName` → `urlName`, and a
/// boolean `isActive` keeps its name. `None` for a name that isn't a
/// getter's (`getaway`, `island`).
fn getter_property_name(getter: &str) -> Option<String> {
    let starts_a_word = |rest: &str| {
        rest.chars()
            .next()
            .is_some_and(|first| !first.is_ascii_lowercase())
    };
    if let Some(rest) = getter.strip_prefix("is")
        && starts_a_word(rest)
    {
        return Some(getter.to_owned());
    }
    let rest = getter.strip_prefix("get")?;
    starts_a_word(rest).then(|| decapitalize_acronym(rest))
}

/// `Title` → `title`; a leading acronym lowered whole, up to the next
/// word's capital: `URL` → `url`, `URLName` → `urlName`.
fn decapitalize_acronym(word: &str) -> String {
    let characters: Vec<char> = word.chars().collect();
    let lowered = match characters
        .iter()
        .position(|char| !char.is_ascii_uppercase())
    {
        None => characters.len(),
        Some(0) => 0,
        Some(1) => 1,
        Some(next_word_lowercase) => next_word_lowercase - 1,
    };
    characters[..lowered]
        .iter()
        .map(char::to_ascii_lowercase)
        .chain(characters[lowered..].iter().copied())
        .collect()
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

/// Which members a walk over a hierarchy collects.
#[derive(Clone, Copy)]
enum Wanted {
    Named(Name),
    All,
}
