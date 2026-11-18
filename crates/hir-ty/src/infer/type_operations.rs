//! What a cast (`as`, `as?`) or a type check (`is`, `!is`) is known to do
//! before it runs, by the Kotlin compiler's rules: a cast to the type the
//! value already has, one no value can pass, one the run time can't check
//! (type arguments are erased), a check whose result is known. Every
//! verdict needs both types known whole; anything less stays silent.

use hir_def::ItemLocation;
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, TypeArgument, TypeRef, kotlin_type_ref};
use syntax::{SyntaxNode, TextRange, named_children};

use super::declarations::Inferrer;
use super::{Reference, TypeOperationFinding, TypeOperationKind};
use crate::members::is_java;
use crate::ty::{ClassType, Nullability, Ty, TypeParameterId};

/// A cast's target or a check's type as written, with what its type
/// arguments say.
struct Written {
    ty: Ty,
    /// Per type argument of the class: `*`.
    stars: Vec<bool>,
}

impl Written {
    fn all_stars(&self) -> bool {
        self.stars.iter().all(|star| *star)
    }
}

/// Whether a class can have subclasses, as the cast rules ask.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassShape {
    Interface,
    /// No subclass can exist.
    Final,
    /// A class that may have subclasses.
    Open,
    /// Not known: an enum, an annotation, an `expect` class, a Kotlin class
    /// a compiler plugin may open (`kotlin-spring` opens annotated ones).
    Unknown,
}

/// Whether a cast's type arguments survive to run time.
enum Erasure {
    Checked,
    Unchecked,
    Unknown,
}

const PRIMITIVE_NUMBERS: [&str; 10] = [
    "Byte", "Short", "Int", "Long", "Float", "Double", "UByte", "UShort", "UInt", "ULong",
];

impl Inferrer<'_> {
    /// `operand as target` / `operand as? target`, `operand` of type
    /// `operand_type`.
    pub(super) fn examine_cast(
        &mut self,
        node: SyntaxNode<'_>,
        operand: SyntaxNode<'_>,
        operand_type: &Ty,
        target: SyntaxNode<'_>,
    ) {
        let Some(operator) = syntax::token(node, "as?").or_else(|| syntax::token(node, "as"))
        else {
            return;
        };
        let safe = self.text_of_token(operator) == "as?";
        let as_type = TextRange::new(operator.start_byte(), node.end_byte());
        let misparsed = super::reassociation::continues_chain(node);
        let examined = !misparsed && self.examinable(operand);
        let kind = examined
            .then(|| self.written_target(target))
            .flatten()
            .and_then(|written| self.cast_verdict(operand_type, &written, safe, operand, node));
        let range = match &kind {
            Some(TypeOperationKind::CastNeverSucceeds) => TextRange::of(operator),
            _ => as_type,
        };
        self.record_type_operation(
            as_type.start,
            kind.map(|kind| TypeOperationFinding { kind, range }),
        );
    }

    /// `operand is target` / `operand !is target`, or a `when` branch's
    /// `is target` on the subject (`test`, the whole check).
    pub(super) fn examine_type_test(
        &mut self,
        test: SyntaxNode<'_>,
        operand: Option<SyntaxNode<'_>>,
        operand_type: &Ty,
        target: SyntaxNode<'_>,
        negated: bool,
    ) {
        let range = TextRange::of(test);
        let misparsed = super::reassociation::continues_chain(test);
        let examinable = !misparsed && operand.is_none_or(|operand| self.examinable(operand));
        let useless = examinable
            && self
                .written_target(target)
                .is_some_and(|written| self.is_upcast(operand_type, &written.ty));
        let finding = useless.then_some(TypeOperationFinding {
            kind: TypeOperationKind::UselessIsCheck { result: !negated },
            range,
        });
        self.record_type_operation(range.start, finding);
    }

    fn cast_verdict(
        &self,
        operand_type: &Ty,
        written: &Written,
        safe: bool,
        operand: SyntaxNode<'_>,
        node: SyntaxNode<'_>,
    ) -> Option<TypeOperationKind> {
        let value = known_value(operand_type)?;
        let target = &written.ty;
        if self.is_useless_cast(&value, target, safe) {
            let removable = TextRange::new(operand.end_byte(), node.end_byte());
            return Some(TypeOperationKind::UselessCast { removable });
        }
        if self.never_succeeds(&value, target) {
            return Some(TypeOperationKind::CastNeverSucceeds);
        }
        match self.erasure(&value, written) {
            Erasure::Unchecked => Some(TypeOperationKind::UncheckedCast {
                from: value,
                to: target.clone(),
            }),
            Erasure::Checked | Erasure::Unknown => None,
        }
    }

    /// The value already has exactly the target type (`as?` of a nullable
    /// value compares with the target made nullable).
    fn is_useless_cast(&self, value: &Ty, target: &Ty, safe: bool) -> bool {
        if target.has_unknown_part() {
            return false;
        }
        let value = upper_bound(value.clone());
        let target = if safe && value.nullability() == Nullability::Nullable {
            target.clone().nullable()
        } else {
            target.clone()
        };
        value == target
    }

    /// Kotlin's rule: no value of `value`'s type is a `target` when the two
    /// are unrelated and one is a final class, or both are classes (not
    /// interfaces: no class extends two). Checked here only for classes
    /// without type arguments; two nullable types (where `null` passes)
    /// and two numbers get other warnings.
    fn never_succeeds(&self, value: &Ty, target: &Ty) -> bool {
        let (Ty::Class(value_class), Ty::Class(target_class)) = (value, target) else {
            return false;
        };
        let generic = !value_class.arguments.is_empty() || !target_class.arguments.is_empty();
        let both_nullable = value_class.nullability != Nullability::NotNull
            && target_class.nullability != Nullability::NotNull;
        if generic || both_nullable {
            return false;
        }
        let both_numbers = self.is_primitive_number(value_class.class)
            && self.is_primitive_number(target_class.class);
        if both_numbers {
            return false;
        }
        let value_shape = self.class_shape(value_class.class);
        let target_shape = self.class_shape(target_class.class);
        let one_final = value_shape == ClassShape::Final || target_shape == ClassShape::Final;
        let both_classes = matches!(value_shape, ClassShape::Final | ClassShape::Open)
            && matches!(target_shape, ClassShape::Final | ClassShape::Open);
        if !one_final && !both_classes {
            return false;
        }
        self.unrelated(value_class.class, target_class.class)
    }

    /// Neither class is the other's subclass, every supertype known.
    fn unrelated(&self, first: ItemLocation, second: ItemLocation) -> bool {
        let complete =
            self.types.hierarchy_complete(first) && self.types.hierarchy_complete(second);
        complete && !self.is_subclass(first, second) && !self.is_subclass(second, first)
    }

    fn is_subclass(&self, class: ItemLocation, of: ItemLocation) -> bool {
        self.types
            .all_supertypes(&self.types.self_type(class))
            .iter()
            .any(|supertype| supertype.class == of)
    }

    /// Whether a value of type `value` is always a `target` — a type check
    /// that can't fail. Only class types are compared: a type parameter
    /// may stand for a nullable type.
    fn is_upcast(&self, value: &Ty, target: &Ty) -> bool {
        let Some(Ty::Class(value_class)) = known_value(value).map(upper_bound) else {
            return false;
        };
        let Ty::Class(target_class) = target else {
            return false;
        };
        let null_excluded = value_class.nullability == Nullability::Nullable
            && target_class.nullability == Nullability::NotNull;
        if null_excluded || target.has_unknown_part() {
            return false;
        }
        let Some(view) = self
            .types
            .all_supertypes(&value_class)
            .into_iter()
            .find(|supertype| supertype.class == target_class.class)
        else {
            return false;
        };
        view.arguments == target_class.arguments
    }

    /// Whether the run time can check a cast of `value` to the target: not
    /// to a type parameter it erases, nor to type arguments a value of
    /// `value`'s type doesn't determine (`Any as List<String>`).
    fn erasure(&self, value: &Ty, written: &Written) -> Erasure {
        match &written.ty {
            Ty::TypeParameter {
                parameter,
                nullability,
            } => self.erasure_to_parameter(value, *parameter, *nullability),
            Ty::Class(target) if target.arguments.is_empty() || written.all_stars() => {
                Erasure::Checked
            }
            Ty::Class(target) => {
                self.erasure_to_class(&value.clone().non_null(), target, &written.stars)
            }
            _ => Erasure::Unknown,
        }
    }

    /// `x as T`: unchecked unless `T` is reified, or `x` is a `T` already
    /// (`T` to `T` or `T?`; `T?` to `T` only when `T` can't be null).
    fn erasure_to_parameter(
        &self,
        value: &Ty,
        parameter: TypeParameterId,
        target_nullability: Nullability,
    ) -> Erasure {
        let declared = hir_def::item_tree(self.db, parameter.owner.container)
            .item(parameter.owner.item)
            .type_parameters
            .get(parameter.index as usize)
            .map(|declared| declared.reified);
        match declared {
            None => return Erasure::Unknown,
            Some(true) => return Erasure::Checked,
            Some(false) => {}
        }
        match value {
            Ty::TypeParameter {
                parameter: own,
                nullability,
            } if *own == parameter => {
                let upcast = *nullability == Nullability::NotNull
                    || target_nullability == Nullability::Nullable;
                if upcast || !self.may_be_null(parameter) {
                    Erasure::Checked
                } else {
                    Erasure::Unchecked
                }
            }
            // Another type parameter may be bounded by this one.
            Ty::TypeParameter { .. } => Erasure::Unknown,
            Ty::Class(_) => Erasure::Unchecked,
            _ => Erasure::Unknown,
        }
    }

    /// `x as C<A>`: checked when `x`'s type is a `C<A>` already, or
    /// determines `C`'s type arguments (a `Map<K, V>` cast to
    /// `MutableMap<K, V>`); unchecked when an argument it doesn't
    /// determine is written (`Any as List<String>`).
    fn erasure_to_class(&self, value: &Ty, target: &ClassType, stars: &[bool]) -> Erasure {
        let Ty::Class(value_class) = value else {
            return Erasure::Unknown;
        };
        let matches_written = |known: &[Ty]| {
            known
                .iter()
                .zip(target.arguments.iter())
                .zip(stars)
                .all(|((known, written), star)| *star || known == written)
        };
        if let Some(view) = self
            .types
            .all_supertypes(value_class)
            .into_iter()
            .find(|supertype| supertype.class == target.class)
        {
            // An upcast; arguments that differ may still fit by variance,
            // which isn't modelled.
            return if matches_written(&view.arguments) {
                Erasure::Checked
            } else {
                Erasure::Unknown
            };
        }
        let Some(determined) = self.determined_arguments(value_class, target.class) else {
            return Erasure::Unknown;
        };
        let mut erasure = Erasure::Checked;
        for ((known, written), star) in determined.iter().zip(target.arguments.iter()).zip(stars) {
            if *star {
                continue;
            }
            match known {
                Some(known) if known == written => {}
                // Different arguments may fit by variance.
                Some(_) => return Erasure::Unknown,
                // `out T` written as `Any?` takes anything.
                None if self.is_nullable_any(written) => return Erasure::Unknown,
                None => erasure = Erasure::Unchecked,
            }
        }
        erasure
    }

    /// What a value of type `value` says `target`'s type arguments are: the
    /// target's supertype of `value`'s class, unified with `value` —
    /// `ArrayList<E>` seen as `List<E>` against `List<String>` makes `E`
    /// `String`. `None` per argument it leaves open (all of them for an
    /// unrelated `value`); `None` overall when unification isn't plain.
    fn determined_arguments(
        &self,
        value: &ClassType,
        target: ItemLocation,
    ) -> Option<Vec<Option<Ty>>> {
        let own = self.types.self_type(target);
        let mut determined: Vec<Option<Ty>> = vec![None; own.arguments.len()];
        let Some(view) = self
            .types
            .all_supertypes(&own)
            .into_iter()
            .find(|supertype| supertype.class == value.class)
        else {
            return Some(determined);
        };
        for (in_view, given) in view.arguments.iter().zip(value.arguments.iter()) {
            match in_view {
                Ty::TypeParameter { parameter, .. } if parameter.owner == target => {
                    let slot = determined.get_mut(parameter.index as usize)?;
                    match slot {
                        Some(earlier) if earlier != given => return None,
                        _ => *slot = Some(given.clone()),
                    }
                }
                other if mentions_parameter_of(other, target) => return None,
                _ => {}
            }
        }
        Some(determined)
    }

    /// The written target, when its type and arguments are all known (a
    /// `*` aside); use-site variance (`out T`) and arguments on an outer
    /// class aren't modelled.
    fn written_target(&self, target: SyntaxNode<'_>) -> Option<Written> {
        let ty = self.written_type(target);
        let stars = match kotlin_type_ref(self.parse, target) {
            TypeRef::Named { segments, .. } => {
                let (last, outer) = segments.split_last()?;
                if outer.iter().any(|segment| !segment.arguments.is_empty()) {
                    return None;
                }
                let mut stars = Vec::with_capacity(last.arguments.len());
                for argument in last.arguments.iter() {
                    match argument {
                        TypeArgument::Star => stars.push(true),
                        TypeArgument::Type { variance, .. } => {
                            if *variance != hir_def::item_tree::Variance::Invariant {
                                return None;
                            }
                            stars.push(false);
                        }
                    }
                }
                stars
            }
            _ => return None,
        };
        let known = match &ty {
            Ty::Class(class) => {
                class.arguments.len() == stars.len()
                    && class
                        .arguments
                        .iter()
                        .zip(&stars)
                        .all(|(argument, star)| *star || !argument.has_unknown_part())
            }
            Ty::TypeParameter { .. } => stars.is_empty(),
            _ => false,
        };
        known.then_some(Written { ty, stars })
    }

    /// Whether the compiler's type for `operand` is the one inferred here:
    /// a name, a property, `this`, a plain string, or a call of a function
    /// without type parameters (a generic one may take its type from the
    /// cast itself).
    pub(super) fn examinable(&self, operand: SyntaxNode<'_>) -> bool {
        match operand.kind() {
            "parenthesized_expression" => named_children(operand)
                .next()
                .is_some_and(|inner| self.examinable(inner)),
            "this_expression" => true,
            "simple_identifier" => self.recorded_at(operand).is_some(),
            "navigation_expression" => {
                let is_reference = syntax::has_token(operand, "::");
                !is_reference
                    && last_name(operand).is_some_and(|name| self.recorded_at(name).is_some())
            }
            "string_literal" => named_children(operand).all(|part| {
                !matches!(
                    part.kind(),
                    "interpolated_identifier" | "interpolated_expression"
                )
            }),
            "call_expression" => self.is_plain_call(operand),
            _ => false,
        }
    }

    /// A call of a function (or constructor of a class) without type
    /// parameters.
    fn is_plain_call(&self, call: SyntaxNode<'_>) -> bool {
        let Some(callee) = named_children(call).next() else {
            return false;
        };
        let name = match callee.kind() {
            "simple_identifier" => Some(callee),
            "navigation_expression" => last_name(callee),
            _ => None,
        };
        let Some(Reference::Declaration(location)) = name
            .and_then(|name| self.recorded_at(name))
            .map(|resolved| resolved.reference)
        else {
            return false;
        };
        hir_def::item_tree(self.db, location.container)
            .item(location.item)
            .type_parameters
            .is_empty()
    }

    fn class_shape(&self, class: ItemLocation) -> ClassShape {
        let tree = hir_def::item_tree(self.db, class.container);
        let item = tree.item(class.item);
        let ItemKind::Class(kind) = item.kind else {
            return ClassShape::Unknown;
        };
        // Kotlin maps `java.lang` classes to its own (`String`, `Integer`).
        let in_java_package = tree
            .package
            .segments()
            .first()
            .is_some_and(|first| first.as_str() == "java");
        if in_java_package || item.modifiers.contains(Modifiers::EXPECT) {
            return ClassShape::Unknown;
        }
        match kind {
            ClassKind::Interface => ClassShape::Interface,
            ClassKind::Object | ClassKind::CompanionObject => ClassShape::Final,
            ClassKind::Enum | ClassKind::Annotation => ClassShape::Unknown,
            ClassKind::Class if is_java(self.db, class) => {
                if item.modifiers.contains(Modifiers::FINAL) {
                    ClassShape::Final
                } else {
                    ClassShape::Open
                }
            }
            ClassKind::Class => {
                let in_workspace = matches!(class.container, hir_def::ItemContainer::File(_));
                let open = [Modifiers::OPEN, Modifiers::ABSTRACT, Modifiers::SEALED]
                    .into_iter()
                    .any(|modifier| item.modifiers.contains(modifier));
                if open {
                    ClassShape::Open
                } else if in_workspace && !tree.annotations(class.item).is_empty() {
                    ClassShape::Unknown
                } else {
                    ClassShape::Final
                }
            }
        }
    }

    /// Whether a type parameter may stand for a nullable type: none of its
    /// bounds excludes `null` (no bound is `Any?`).
    fn may_be_null(&self, parameter: TypeParameterId) -> bool {
        self.types
            .type_parameter_bounds(parameter)
            .iter()
            .all(|bound| !bound.is_never_null())
    }

    fn is_primitive_number(&self, class: ItemLocation) -> bool {
        PRIMITIVE_NUMBERS.iter().any(
            |name| matches!(self.types.builtin(name), Ty::Class(number) if number.class == class),
        )
    }

    fn is_nullable_any(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Class(class) => {
                class.nullability == Nullability::Nullable && self.types.is_any(class)
            }
            _ => false,
        }
    }

    fn text_of_token(&self, token: SyntaxNode<'_>) -> &str {
        &self.parse.text()[token.byte_range()]
    }
}

/// `ty` when it's known whole and a class or type parameter: a function
/// type, `Nothing` and Java types nested in arguments (`List<String!>`)
/// aren't compared.
fn known_value(ty: &Ty) -> Option<Ty> {
    let known = match ty {
        Ty::Class(class) => class.arguments.iter().all(is_known_argument),
        Ty::TypeParameter { .. } => true,
        Ty::Function(_) | Ty::Nothing { .. } | Ty::Unknown => false,
    };
    known.then(|| ty.clone())
}

fn is_known_argument(argument: &Ty) -> bool {
    match argument {
        Ty::Class(class) => {
            class.nullability != Nullability::Platform
                && class.arguments.iter().all(is_known_argument)
        }
        Ty::TypeParameter { nullability, .. } => *nullability != Nullability::Platform,
        Ty::Function(_) | Ty::Nothing { .. } | Ty::Unknown => false,
    }
}

/// A Java type (`String!`) as its widest Kotlin reading: `String?`.
fn upper_bound(ty: Ty) -> Ty {
    match ty.nullability() {
        Nullability::Platform => ty.with_nullability(Nullability::Nullable),
        _ => ty,
    }
}

fn mentions_parameter_of(ty: &Ty, owner: ItemLocation) -> bool {
    match ty {
        Ty::TypeParameter { parameter, .. } => parameter.owner == owner,
        Ty::Class(class) => class
            .arguments
            .iter()
            .any(|argument| mentions_parameter_of(argument, owner)),
        Ty::Function(_) => true,
        Ty::Nothing { .. } | Ty::Unknown => false,
    }
}

/// The member name of `receiver.name`.
fn last_name(navigation: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let suffix = named_children(navigation).last()?;
    named_children(suffix).find(|child| child.kind() == "simple_identifier")
}
