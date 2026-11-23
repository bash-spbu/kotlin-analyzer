//! Whether a value fits where a type is expected, binding a callee's type
//! parameters from the values passed, and the common type of branches.

use hir_def::item_tree as tree;
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers};

use crate::members::{Types, class_substitution};
use crate::ty::{ClassType, FunctionType, Nullability, Substitution, Ty, TypeParameterId};

/// How well a value fits an expected type.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Fit {
    No,
    /// Can't tell (something is unknown, or the rules involved aren't
    /// modelled): treated as fitting, never reported.
    Unknown,
    Yes,
}

impl Fit {
    pub fn and(self, other: Fit) -> Fit {
        self.min(other)
    }
}

impl Types<'_> {
    /// Whether a value of type `value` may be used where `expected` is.
    pub fn fits(&self, value: &Ty, expected: &Ty) -> Fit {
        if value.is_unknown() || expected.is_unknown() {
            return Fit::Unknown;
        }
        let null_into_non_null = value.nullability() == Nullability::Nullable
            && expected.nullability() == Nullability::NotNull
            && !matches!(expected, Ty::TypeParameter { .. });
        if null_into_non_null {
            return Fit::No;
        }
        match (value, expected) {
            (Ty::Nothing { .. }, _) => Fit::Yes,
            (
                Ty::TypeParameter {
                    parameter: value_parameter,
                    ..
                },
                Ty::TypeParameter {
                    parameter: expected_parameter,
                    ..
                },
            ) if value_parameter == expected_parameter => Fit::Yes,
            (_, Ty::TypeParameter { .. }) => Fit::Unknown,
            (Ty::TypeParameter { parameter, .. }, Ty::Class(expected_class)) => {
                if self.is_any(expected_class) {
                    return Fit::Yes;
                }
                let bounds = self.type_parameter_bounds(*parameter);
                if bounds
                    .iter()
                    .any(|bound| self.fits(bound, expected) == Fit::Yes)
                {
                    Fit::Yes
                } else {
                    Fit::Unknown
                }
            }
            (Ty::Function(value_function), Ty::Function(expected_function)) => {
                if arity(value_function) == arity(expected_function) {
                    Fit::Yes
                } else {
                    Fit::No
                }
            }
            (Ty::Function(_), Ty::Class(expected_class)) => {
                if self.is_any(expected_class) {
                    Fit::Yes
                } else {
                    // A functional interface (SAM conversion), or `FunctionN`.
                    Fit::Unknown
                }
            }
            (Ty::Class(_), Ty::Function(_)) => Fit::Unknown,
            (Ty::Class(value_class), Ty::Class(expected_class)) => {
                self.class_fits(value_class, expected_class)
            }
            (_, Ty::Nothing { .. }) => Fit::No,
            _ => Fit::Unknown,
        }
    }

    fn class_fits(&self, value: &ClassType, expected: &ClassType) -> Fit {
        if value.class == expected.class || self.is_any(expected) {
            return Fit::Yes;
        }
        let is_supertype = self
            .hierarchy(value.class)
            .iter()
            .any(|supertype| supertype.class == expected.class);
        if is_supertype {
            return Fit::Yes;
        }
        // A Java parameter typed `java.util.List` takes a read-only
        // `kotlin.collections.List` as well: it's `(Mutable)List!`.
        if expected.nullability == Nullability::Platform
            && let Some(read_only) = self.read_only_collection(expected)
            && all_supertypes_contain(&self.all_supertypes(value), read_only)
        {
            return Fit::Yes;
        }
        // A supertype that didn't resolve may be the expected one.
        if !self.hierarchy_complete(value.class) {
            return Fit::Unknown;
        }
        Fit::No
    }

    /// `kotlin.collections.List` for `MutableList` (and so on).
    pub(crate) fn read_only_collection(
        &self,
        mutable: &ClassType,
    ) -> Option<hir_def::ItemLocation> {
        if let Some(known) = self.read_only_collections.borrow().get(&mutable.class) {
            return *known;
        }
        let found = self.find_read_only_collection(mutable);
        self.read_only_collections
            .borrow_mut()
            .insert(mutable.class, found);
        found
    }

    fn find_read_only_collection(&self, mutable: &ClassType) -> Option<hir_def::ItemLocation> {
        let tree = hir_def::item_tree(self.db, mutable.class.container);
        let item = tree.item(mutable.class.item);
        let read_only_name = item.name.as_str().strip_prefix("Mutable")?;
        let parent = item.parent.map(|parent| tree.item(parent).name.as_str());
        let qualified = match parent {
            // `MutableMap.MutableEntry` → `Map.Entry`.
            Some(parent) => format!(
                "{}.{read_only_name}",
                parent.strip_prefix("Mutable").unwrap_or(parent)
            ),
            None => read_only_name.to_owned(),
        };
        match crate::lower::builtin(
            self.db,
            self.from,
            crate::lower::KnownPackage::KotlinCollections,
            &qualified,
            Vec::new(),
        ) {
            Ty::Class(read_only) => Some(read_only.class),
            _ => None,
        }
    }

    pub(crate) fn is_any(&self, class: &ClassType) -> bool {
        matches!(self.builtin("Any"), Ty::Class(any) if any.class == class.class)
    }

    /// Binds the callee's `parameters` that `expected` mentions from the
    /// value passed for it (`List<T>` given `ArrayList<String>` binds `T`
    /// to `String`).
    /// Two lower bounds of a type parameter as one: the wider of two
    /// classes that fit one another, else their common supertype — only
    /// for classes without type arguments (`Circle`, `Square`: `Shape`), and
    /// with the widest nullability. `None` when that isn't known: the first
    /// bound stays.
    fn joined_lower_bounds(&self, known: &Ty, added: &Ty) -> Option<Ty> {
        let (Ty::Class(known_class), Ty::Class(added_class)) = (known, added) else {
            return None;
        };
        if known_class.class == added_class.class {
            return None;
        }
        let without_arguments =
            known_class.arguments.is_empty() && added_class.arguments.is_empty();
        if !without_arguments {
            return None;
        }
        let nullable = known.nullability() == Nullability::Nullable
            || added.nullability() == Nullability::Nullable;
        let (known_base, added_base) = (known.clone().non_null(), added.clone().non_null());
        let base = if self.fits(&added_base, &known_base) == Fit::Yes {
            known_base
        } else if self.fits(&known_base, &added_base) == Fit::Yes {
            added_base
        } else {
            match self.common_type(&[known_base, added_base]) {
                Ty::Unknown | Ty::Error => return None,
                common => common,
            }
        };
        Some(if nullable { base.nullable() } else { base })
    }

    pub fn unify(
        &self,
        expected: &Ty,
        value: &Ty,
        parameters: &[TypeParameterId],
        substitution: &mut Substitution,
    ) {
        self.unify_at_depth(expected, value, parameters, substitution, 0);
    }

    fn unify_at_depth(
        &self,
        expected: &Ty,
        value: &Ty,
        parameters: &[TypeParameterId],
        substitution: &mut Substitution,
        depth: usize,
    ) {
        if depth > 8 || value.is_unknown() {
            return;
        }
        match expected {
            Ty::TypeParameter {
                parameter,
                nullability,
            } if parameters.contains(parameter) => {
                if matches!(value, Ty::Nothing { .. }) {
                    return;
                }
                // `T?` given `X?` binds `T` to `X`; so does `T : Any` (a
                // parameter with only non-null bounds is never nullable).
                let bounds = self.type_parameter_bounds(*parameter);
                let never_nullable = !bounds.is_empty()
                    && bounds
                        .iter()
                        .all(|bound| bound.nullability() == Nullability::NotNull);
                let bound = if nullability == &Nullability::Nullable || never_nullable {
                    value.clone().without_question_mark()
                } else {
                    value.clone()
                };
                // Bound before by a value passed as the parameter itself
                // (`listOf(circle, square)`): both are lower bounds, and the
                // parameter is what they have in common. Deeper — a lambda's
                // receiver or parameter is an upper bound — the first stays.
                let lower_bound = depth == 0;
                if let Some(joined) = substitution
                    .get(*parameter)
                    .filter(|_| lower_bound)
                    .and_then(|known| self.joined_lower_bounds(known, &bound))
                {
                    substitution.insert(*parameter, joined);
                    return;
                }
                substitution.bind(*parameter, bound);
            }
            Ty::Class(expected_class) => {
                if expected_class.arguments.is_empty() {
                    return;
                }
                if let Ty::Function(_) = value {
                    // A lambda passed for a functional interface binds through
                    // its single method: `Rows<T>` given `{ row, _ -> 1 }`.
                    if let Some(function) = self.functional_interface_type(expected_class) {
                        self.unify_at_depth(
                            &Ty::Function(Box::new(function)),
                            value,
                            parameters,
                            substitution,
                            depth + 1,
                        );
                    }
                    return;
                }
                let Some(instance) = self.as_supertype(value, expected_class.class) else {
                    return;
                };
                for (expected_argument, value_argument) in expected_class
                    .arguments
                    .iter()
                    .zip(instance.arguments.iter())
                {
                    self.unify_at_depth(
                        expected_argument,
                        value_argument,
                        parameters,
                        substitution,
                        depth + 1,
                    );
                }
            }
            Ty::Function(expected_function) => {
                let Ty::Function(value_function) = value else {
                    return;
                };
                if let (Some(expected_receiver), Some(value_receiver)) =
                    (&expected_function.receiver, &value_function.receiver)
                {
                    self.unify_at_depth(
                        expected_receiver,
                        value_receiver,
                        parameters,
                        substitution,
                        depth + 1,
                    );
                }
                for (expected_parameter, value_parameter) in expected_function
                    .parameters
                    .iter()
                    .zip(value_function.parameters.iter())
                {
                    self.unify_at_depth(
                        expected_parameter,
                        value_parameter,
                        parameters,
                        substitution,
                        depth + 1,
                    );
                }
                self.unify_at_depth(
                    &expected_function.result,
                    &value_function.result,
                    parameters,
                    substitution,
                    depth + 1,
                );
            }
            _ => {}
        }
    }

    /// The type of an expression whose value is one of `branches`.
    pub fn common_type(&self, branches: &[Ty]) -> Ty {
        let mut nullable = false;
        // A Java branch (`poll(): E!`) leaves the result's nullability
        // unknown, unless another branch makes it nullable.
        let mut platform = false;
        let mut common: Option<Ty> = None;
        for branch in branches {
            match branch {
                Ty::Nothing { nullable: true } => nullable = true,
                Ty::Nothing { nullable: false } => {}
                other => {
                    match other.nullability() {
                        Nullability::Nullable => nullable = true,
                        Nullability::Platform => platform = true,
                        Nullability::NotNull => {}
                    }
                    let other = other.clone().non_null();
                    common = Some(match common {
                        None => other,
                        Some(known) => self.common_supertype(&known, &other),
                    });
                }
            }
        }
        match (common, nullable, platform) {
            (Some(common), true, _) => common.nullable(),
            (Some(common), false, true) => common.with_nullability(Nullability::Platform),
            (Some(common), false, false) => common,
            (None, true, _) => Ty::Nothing { nullable: true },
            (None, false, _) => Ty::Nothing { nullable: false },
        }
    }

    /// Whether `value`, seen as `wider`'s class, has `wider`'s arguments.
    fn arguments_agree(&self, value: &Ty, wider: &Ty) -> bool {
        let Ty::Class(wider_class) = wider else {
            return true;
        };
        if wider_class.arguments.is_empty() {
            return true;
        }
        self.as_supertype(value, wider_class.class)
            .is_some_and(|as_wider| as_wider.arguments == wider_class.arguments)
    }

    fn common_supertype(&self, first: &Ty, second: &Ty) -> Ty {
        if first == second {
            return first.clone();
        }
        if first.is_unknown() || second.is_unknown() {
            return Ty::Unknown;
        }
        // An unknown argument fits anything: `Pair<T, *>` "fits" `Pair<*,
        // Failure>`, yet their common type isn't the second. Only types
        // known throughout may stand for both.
        let known_throughout = !first.has_unknown_part() && !second.has_unknown_part();
        // `fits` compares classes only: `List<Int>` "fits" `List<String>`.
        // One stands for both only when the arguments agree too.
        if known_throughout
            && self.fits(first, second) == Fit::Yes
            && self.arguments_agree(first, second)
        {
            return second.clone();
        }
        if known_throughout
            && self.fits(second, first) == Fit::Yes
            && self.arguments_agree(second, first)
        {
            return first.clone();
        }
        let (Ty::Class(first_class), Ty::Class(second_class)) = (first, second) else {
            return Ty::Unknown;
        };
        let second_supertypes = self.all_supertypes(second_class);
        let shared: Vec<ClassType> = self
            .all_supertypes(first_class)
            .into_iter()
            .filter_map(|supertype| {
                let other = second_supertypes
                    .iter()
                    .find(|other| other.class == supertype.class)?;
                Some(agreeing_arguments(supertype, other))
            })
            .collect();
        let mut least = shared.iter().filter(|candidate| {
            !shared.iter().any(|other| {
                other.class != candidate.class
                    && self
                        .all_supertypes(other)
                        .iter()
                        .any(|above| above.class == candidate.class)
            })
        });
        // Two least common supertypes (`Comparable` and `Serializable`) make
        // the type both at once, which a `Ty` can't say: unknown.
        match (least.next(), least.next()) {
            (Some(only), None) => Ty::Class(only.clone()),
            _ => Ty::Unknown,
        }
    }

    /// The function a functional interface stands for (`Runnable` → `()
    /// -> Unit`, `Comparator<T>` → `(T, T) -> Int`): its single abstract
    /// method, through the type's arguments.
    pub fn functional_interface_type(&self, class_type: &ClassType) -> Option<FunctionType> {
        let class_tree = hir_def::item_tree(self.db, class_type.class.container);
        let class_item = class_tree.item(class_type.class.item);
        if class_item.kind != ItemKind::Class(ClassKind::Interface) {
            return None;
        }
        let mut abstract_methods = Vec::new();
        for supertype in self.all_supertypes(class_type) {
            let tree = hir_def::item_tree(self.db, supertype.class.container);
            for child in tree.item(supertype.class.item).children.iter() {
                let item = tree.item(*child);
                let is_abstract_method = item.kind == ItemKind::Function
                    && item.modifiers.contains(Modifiers::ABSTRACT)
                    && !is_object_method(item);
                if is_abstract_method {
                    abstract_methods.push((
                        hir_def::ItemLocation {
                            container: supertype.class.container,
                            item: *child,
                        },
                        class_substitution(&supertype),
                    ));
                }
            }
        }
        let [(method, substitution)] = &abstract_methods[..] else {
            return None;
        };
        let signature = self.signature(crate::signature::Callable::Function(*method))?;
        Some(FunctionType {
            receiver: None,
            parameters: signature
                .parameters
                .iter()
                .map(|parameter| parameter.ty.substitute(substitution))
                .collect(),
            result: signature.result.substitute(substitution),
            suspend: false,
            nullability: Nullability::NotNull,
        })
    }
}

fn all_supertypes_contain(supertypes: &[ClassType], class: hir_def::ItemLocation) -> bool {
    supertypes.iter().any(|supertype| supertype.class == class)
}

/// `equals`, `hashCode`, `toString` an interface may redeclare abstractly
/// without making them its single method.
fn is_object_method(item: &tree::Item) -> bool {
    matches!(item.name.as_str(), "equals" | "hashCode" | "toString")
}

/// How many values a function takes, its receiver included.
fn arity(function: &FunctionType) -> usize {
    function.parameters.len() + usize::from(function.receiver.is_some())
}

/// One class seen as a supertype of two types: its arguments where both
/// agree, unknown where they don't (`List<Int>` and `List<String>`).
fn agreeing_arguments(first: ClassType, second: &ClassType) -> ClassType {
    let arguments = first
        .arguments
        .iter()
        .zip(second.arguments.iter())
        .map(|(first, second)| {
            if first == second {
                first.clone()
            } else {
                Ty::Unknown
            }
        })
        .collect();
    ClassType { arguments, ..first }
}
