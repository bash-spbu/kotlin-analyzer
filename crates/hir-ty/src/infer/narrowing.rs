//! Smart casts: what a condition tells about locals when it holds, and
//! when it doesn't (`x != null`, `x is String`, combined with `&&`, `||`,
//! `!`).

use hir_def::ItemLocation;
use hir_def::item_tree::{Returns, Signature};
use syntax::ast::kotlin::TypeTest;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::Inferrer;
use super::property_paths::{PropertyRead, property_name};
use super::{LocalId, NullCheckKind, UnsafeCall};
use crate::ty::{ClassType, Nullability, Ty, TypeParameterId};

/// Members every value has, which Kotlin also offers on a nullable
/// receiver (`Any?.toString()`, `Any?.hashCode()`).
const NULL_SAFE_MEMBERS: &[&str] = &["equals", "hashCode", "toString"];

/// What a smart cast narrows: a local, or a stable property read from a
/// stable value (`order.status`, the `status` of `this`), which the
/// compiler smart-casts as it does a local.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Narrowed {
    Local(LocalId),
    Property(PropertyPathId),
    /// `this` of the implicit receiver at this depth of the receiver stack
    /// (`if (this !is Circle) return`).
    Receiver(u32),
}

/// A stable property read from a stable value, numbered per body.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct PropertyPathId(pub(super) u32);

/// The locals a condition narrows, and to what.
#[derive(Clone, Default)]
pub(super) struct Narrowing {
    pub(super) when_true: Vec<(Narrowed, Ty)>,
    pub(super) when_false: Vec<(Narrowed, Ty)>,
}

impl Narrowing {
    fn negated(self) -> Narrowing {
        Narrowing {
            when_true: self.when_false,
            when_false: self.when_true,
        }
    }
}

impl Inferrer<'_> {
    /// Infers a condition and what it narrows.
    pub(super) fn condition(&mut self, node: SyntaxNode<'_>) -> Narrowing {
        match node.kind() {
            "parenthesized_expression" => match named_children(node).next() {
                Some(inner) => self.condition(inner),
                None => Narrowing::default(),
            },
            "prefix_expression" if has_token(node, "!") => match named_children(node).last() {
                Some(operand) => self.condition(operand).negated(),
                None => Narrowing::default(),
            },
            "conjunction_expression" => {
                let mut operands = named_children(node);
                let (Some(left), Some(right)) = (operands.next(), operands.next()) else {
                    return Narrowing::default();
                };
                let left = self.condition(left);
                // The right side runs only where the left held.
                let (right, _) = self.fork(|this| {
                    this.narrow(&left.when_true);
                    this.condition(right)
                });
                Narrowing {
                    when_false: self.join_narrowings(&left.when_false, &right.when_false),
                    when_true: left.when_true.into_iter().chain(right.when_true).collect(),
                }
            }
            "disjunction_expression" => {
                let mut operands = named_children(node);
                let (Some(left), Some(right)) = (operands.next(), operands.next()) else {
                    return Narrowing::default();
                };
                let left = self.condition(left);
                // The right side runs only where the left didn't hold.
                let (right, _) = self.fork(|this| {
                    this.narrow(&left.when_false);
                    this.condition(right)
                });
                Narrowing {
                    when_true: self.join_narrowings(&left.when_true, &right.when_true),
                    when_false: left
                        .when_false
                        .into_iter()
                        .chain(right.when_false)
                        .collect(),
                }
            }
            "equality_expression" => self.null_check(node),
            "check_expression" => self.type_check(node),
            "comparison_expression" => {
                for operand in named_children(node) {
                    self.expression(operand, None);
                }
                Narrowing::default()
            }
            "call_expression" => {
                self.expression(node, None);
                self.contract_condition(node)
            }
            // `val ready = x != null; if (ready) x.f()`.
            "simple_identifier" => {
                self.expression(node, None);
                self.lookup_value_local(self.name(node))
                    .and_then(|local| self.condition_aliases.get(&local).cloned())
                    .unwrap_or_default()
            }
            _ => {
                self.expression(node, None);
                Narrowing::default()
            }
        }
    }

    /// What a call's contract says where it gave `true` and where `false`
    /// (`!s.isNullOrEmpty()`): `returns(true)`'s, `returns(false)`'s, and
    /// `returns()`'s in both.
    fn contract_condition(&self, call: SyntaxNode<'_>) -> Narrowing {
        let Some(called) = self.called_contract(call) else {
            return Narrowing::default();
        };
        Narrowing {
            when_true: self.contract_narrowings(&called, |returns| {
                matches!(returns, Returns::Normally | Returns::Value(true))
            }),
            when_false: self.contract_narrowings(&called, |returns| {
                matches!(returns, Returns::Normally | Returns::Value(false))
            }),
        }
    }

    /// `x != null` / `x == null`.
    fn null_check(&mut self, node: SyntaxNode<'_>) -> Narrowing {
        let operands: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let types: Vec<Ty> = operands
            .iter()
            .map(|operand| self.expression(*operand, None))
            .collect();
        let [left, right] = operands[..] else {
            return Narrowing::default();
        };
        let is_not_equal = has_token(node, "!=") || has_token(node, "!==");
        let compared = match (left.kind(), right.kind()) {
            (_, "null_literal") => Some((left, &types[0])),
            ("null_literal", _) => Some((right, &types[1])),
            _ => None,
        };
        if let Some((value, value_type)) = compared {
            self.check_null_check(
                value,
                value_type,
                NullCheckKind::Comparison {
                    always: is_not_equal,
                },
                TextRange::of(node),
            );
        }
        let checked = match (left.kind(), right.kind()) {
            (_, "null_literal") => left,
            ("null_literal", _) => right,
            // `x == Value` with a value that can't be null: `x` isn't
            // null where they're equal (`direction != INCOMING` false,
            // `order?.status == PROCESSING` true).
            _ => {
                let compared_with_value =
                    match (self.excludes_null(&types[0]), self.excludes_null(&types[1])) {
                        (_, true) => self.not_null_chain(left),
                        (true, false) => self.not_null_chain(right),
                        (false, false) => Vec::new(),
                    };
                let not_null = compared_with_value;
                let equal = has_token(node, "==") || has_token(node, "===");
                return if equal && !is_not_equal {
                    Narrowing {
                        when_true: not_null,
                        ..Narrowing::default()
                    }
                } else {
                    Narrowing {
                        when_false: not_null,
                        ..Narrowing::default()
                    }
                };
            }
        };
        let not_null = self.not_null_chain(checked);
        if is_not_equal {
            Narrowing {
                when_true: not_null,
                ..Narrowing::default()
            }
        } else {
            Narrowing {
                when_false: not_null,
                ..Narrowing::default()
            }
        }
    }

    /// `x is T` / `x !is T` (and `x in range`, which narrows nothing).
    fn type_check(&mut self, node: SyntaxNode<'_>) -> Narrowing {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&operand, rest)) = children.split_first() else {
            return Narrowing::default();
        };
        let operand_type = self.expression(operand, None);
        let test = TypeTest::of(node);
        let is_type_check = test != TypeTest::None;
        if !is_type_check {
            for other in rest {
                self.expression(*other, None);
            }
            return Narrowing::default();
        }
        let Some(target) = rest
            .iter()
            .find(|child| syntax::ast::kotlin::is_type(**child))
        else {
            return Narrowing::default();
        };
        let negated = test == TypeTest::IsNot;
        self.examine_type_test(node, Some(operand), &operand_type, *target, negated);
        let target_type = self.written_type(*target);
        let Some(subject) = self.narrowable(operand) else {
            return Narrowing::default();
        };
        let narrowed = vec![(
            subject,
            self.smart_cast(&self.narrowed_type(subject), target_type),
        )];
        if test == TypeTest::IsNot {
            Narrowing {
                when_false: narrowed,
                ..Narrowing::default()
            }
        } else {
            Narrowing {
                when_true: narrowed,
                ..Narrowing::default()
            }
        }
    }
}

impl Inferrer<'_> {
    /// What `x is target` makes a local of type `current`: the more
    /// specific of the two when one is a subtype of the other. Unrelated
    /// types would make it both at once, which a `Ty` can't say: unknown.
    pub(super) fn smart_cast(&self, current: &Ty, target: Ty) -> Ty {
        if let Ty::TypeParameter { parameter, .. } = &target {
            return self.smart_cast_to_parameter(current, *parameter, &target);
        }
        let Some(target_class) = target.class().map(|class| class.class) else {
            return target.non_null();
        };
        let holders = self.types.member_holders(current);
        if holders.is_empty() {
            return target.non_null();
        }
        if self.types.as_supertype(current, target_class).is_some() {
            return current.clone().non_null();
        }
        let target_is_narrower = holders
            .iter()
            .all(|holder| self.types.as_supertype(&target, holder.class).is_some());
        if !target_is_narrower {
            return Ty::Unknown;
        }
        match target {
            Ty::Class(class) => Ty::Class(self.with_inferred_arguments(&holders, class)).non_null(),
            other => other.non_null(),
        }
    }

    /// `x is T` for a type parameter `T`: `T` itself when `x` is already a
    /// `T` or is `Any` (every `T` is narrower); else `x` is both its type
    /// and `T` (`Node & T`), which a `Ty` can't say: unknown.
    fn smart_cast_to_parameter(&self, current: &Ty, parameter: TypeParameterId, target: &Ty) -> Ty {
        let already_the_parameter = matches!(
            current,
            Ty::TypeParameter { parameter: own, .. } if *own == parameter
        );
        let is_any = matches!(current, Ty::Class(class) if self.types.is_any(class));
        if already_the_parameter || is_any {
            return target.clone().non_null();
        }
        Ty::Unknown
    }

    /// `x is Value` for `x: ReferenceOr<Schema>`: a generic class tested
    /// without arguments gets the ones `x`'s type gives it (`Value<A> :
    /// ReferenceOr<A>`, so `Value<Schema>`); what it can't tell is unknown.
    fn with_inferred_arguments(&self, holders: &[ClassType], target: ClassType) -> ClassType {
        let declared = self.types.self_type(target.class);
        let written_arguments = target.arguments.len() == declared.arguments.len()
            && target
                .arguments
                .iter()
                .any(|argument| !argument.is_unknown() && !declared.arguments.contains(argument));
        if declared.arguments.is_empty() || written_arguments {
            return target;
        }
        let mut inferred: Vec<Ty> = vec![Ty::Unknown; declared.arguments.len()];
        for holder in holders {
            let Some(as_holder) = self
                .types
                .as_supertype(&Ty::Class(declared.clone()), holder.class)
            else {
                continue;
            };
            for (through_target, known) in as_holder.arguments.iter().zip(holder.arguments.iter()) {
                if let Some(index) = declared
                    .arguments
                    .iter()
                    .position(|own| own == through_target)
                {
                    inferred[index] = known.clone();
                }
            }
        }
        ClassType {
            arguments: inferred.into_boxed_slice(),
            ..target
        }
    }

    /// After a statement `require(x != null)`, `check(x is T)` or
    /// `requireNotNull(x)` — a standard-library call that throws otherwise —
    /// what follows sees its condition hold.
    /// What a call's contract says holds once it returned: `x` after
    /// `requireNotNull(x)` or `require(x is T)`.
    pub(super) fn narrow_after_contract(&mut self, value: SyntaxNode<'_>) -> CallEnd {
        let Some(called) = self.called_contract(value) else {
            return CallEnd::Returns;
        };
        let narrowings = self.contract_narrowings(&called, |returns| returns == Returns::Normally);
        // A lambda run in place ran on the flow here: where it ended is
        // where the call returns.
        for lambda in called.lambdas_surely_run() {
            if let Some(end) = self.lambda_ends.get(&(lambda.start_byte() as u32)) {
                // What it knew of its own receiver (`apply { next!! }`)
                // isn't known of whatever is at that depth next.
                let mut end = end.clone();
                let outer_receivers = self.receivers.len();
                end.retain(|subject| !self.reads_receiver_from(subject, outer_receivers));
                self.flow = end;
            }
        }
        self.narrow(&narrowings);
        if self.flow.is_dead() {
            CallEnd::Never
        } else {
            CallEnd::Returns
        }
    }

    /// What a condition known to hold says, read without inferring it again
    /// (it already was, as an argument): `x != null`, `x is T`, joined by
    /// `&&`.
    pub(super) fn asserted(&self, condition: SyntaxNode<'_>) -> Vec<(Narrowed, Ty)> {
        match condition.kind() {
            "parenthesized_expression" => named_children(condition)
                .next()
                .map(|inner| self.asserted(inner))
                .unwrap_or_default(),
            "conjunction_expression" => named_children(condition)
                .flat_map(|operand| self.asserted(operand))
                .collect(),
            // `require(!s.isNullOrBlank())`: what the call's contract says
            // where it gave `false` (or `true`, not negated).
            "call_expression" => self
                .called_contract(condition)
                .map(|called| {
                    self.contract_narrowings(&called, |returns| {
                        matches!(returns, Returns::Normally | Returns::Value(true))
                    })
                })
                .unwrap_or_default(),
            "prefix_expression" if has_token(condition, "!") => named_children(condition)
                .last()
                .and_then(|operand| self.called_contract(operand))
                .map(|called| {
                    self.contract_narrowings(&called, |returns| {
                        matches!(returns, Returns::Normally | Returns::Value(false))
                    })
                })
                .unwrap_or_default(),
            "equality_expression" if has_token(condition, "!=") => {
                let operands: Vec<SyntaxNode<'_>> = named_children(condition).collect();
                match operands[..] {
                    [checked, other] | [other, checked] if other.kind() == "null_literal" => {
                        self.not_null_chain(checked)
                    }
                    _ => Vec::new(),
                }
            }
            "check_expression" if TypeTest::of(condition) == TypeTest::Is => {
                let mut children = named_children(condition);
                let Some(operand) = children.next() else {
                    return Vec::new();
                };
                let target = children.find(|child| syntax::ast::kotlin::is_type(*child));
                match (self.narrowable(operand), target) {
                    (Some(subject), Some(target)) => {
                        let target = self.written_type(target);
                        vec![(
                            subject,
                            self.smart_cast(&self.narrowed_type(subject), target),
                        )]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// What `node` not being null says isn't null: `node` itself, when a
    /// smart cast can narrow it, and each value of its chain a safe call
    /// was made on (`x?.y` isn't null only when `x` isn't), up to the name
    /// the chain starts from.
    pub(super) fn not_null_chain(&self, node: SyntaxNode<'_>) -> Vec<(Narrowed, Ty)> {
        let mut found: Vec<Narrowed> = self.narrowable(node).into_iter().collect();
        let mut current = node;
        let mut safe = false;
        loop {
            match current.kind() {
                "parenthesized_expression" | "call_expression" => {
                    let Some(inner) = named_children(current).next() else {
                        break;
                    };
                    current = inner;
                }
                "navigation_expression" => {
                    let (Some(receiver), Some(suffix)) = (
                        named_children(current).next(),
                        named_children(current).last(),
                    ) else {
                        break;
                    };
                    if has_token(suffix, "?.") {
                        safe = true;
                        found.extend(self.narrowable(receiver));
                    } else if has_safe_step(receiver) {
                        // `x?.y.orEmpty()`: a call taking `x?.y` even when
                        // it's null — its value says nothing of `x`.
                        break;
                    }
                    current = receiver;
                }
                "simple_identifier" => {
                    if safe {
                        found.extend(self.narrowable(current));
                    }
                    break;
                }
                _ => break,
            }
        }
        let mut unique: Vec<Narrowed> = Vec::new();
        for narrowed in found {
            if !unique.contains(&narrowed) {
                unique.push(narrowed);
            }
        }
        unique
            .into_iter()
            .map(|narrowed| (narrowed, self.narrowed_type(narrowed).non_null()))
            .collect()
    }
}

/// Whether a call returns: not when a lambda it surely runs in place
/// always leaves the function around (`apply { return x }`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CallEnd {
    Returns,
    Never,
}

/// How whether a value may be null where it's used is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum NullKnowledge {
    /// Its type says it: a local or a stable property after the data
    /// flow's smart casts, a call, a property never smart-cast.
    Followed,
    /// Smart casts that would apply to it aren't followed (an `open`
    /// property's, one read from a `var`), or the classpath is partial:
    /// only a check nowhere around tells.
    Unfollowed,
}

impl Inferrer<'_> {
    /// Records `receiver.member` (with a plain `.`) as unsafe when the
    /// receiver's type is nullable, the member is the class's own (not an
    /// extension, which may take a nullable receiver), and the receiver
    /// isn't smart-cast — by the data flow where it follows the receiver;
    /// else by nothing in the declaration around (see
    /// `validation::unguarded`), told once inference is done.
    pub(super) fn check_receiver_not_null(
        &mut self,
        receiver: SyntaxNode<'_>,
        receiver_type: &Ty,
        name: SyntaxNode<'_>,
        member: ItemLocation,
    ) {
        if receiver_type.nullability() != Nullability::Nullable
            || matches!(receiver_type, Ty::Nothing { .. })
        {
            return;
        }
        if NULL_SAFE_MEMBERS.contains(&self.text(name)) || !self.is_own_member(member) {
            return;
        }
        let call = UnsafeCall {
            range: TextRange::of(name),
            receiver: receiver_type.clone(),
        };
        match self.null_knowledge(receiver) {
            NullKnowledge::Followed => self.record_unsafe_call(call),
            // Whether anything around could have narrowed it is told once
            // every name in the body is resolved.
            NullKnowledge::Unfollowed => self.record_unfollowed_receiver(receiver, call),
        }
    }

    /// Whether no value of `ty` is null: a non-null class or function type,
    /// or a type parameter every value of which is (`V : Any`).
    pub(super) fn excludes_null(&self, ty: &Ty) -> bool {
        match ty {
            Ty::TypeParameter {
                parameter,
                nullability: Nullability::NotNull,
            } => self
                .types
                .type_parameter_bounds(*parameter)
                .iter()
                .any(|bound| bound.is_never_null()),
            _ => ty.is_never_null(),
        }
    }

    /// Whether `value`'s type here says for certain whether it may be null:
    /// a local's or a stable property's, after the smart casts the data
    /// flow gives it; a call's, or a property's the compiler never
    /// smart-casts, as it is — with the whole classpath (a missing
    /// library's contract would have narrowed a local).
    pub(super) fn null_knowledge(&self, value: SyntaxNode<'_>) -> NullKnowledge {
        let is_local = value.kind() == "simple_identifier"
            && matches!(
                self.recorded_at(value).map(|resolved| resolved.reference),
                Some(super::Reference::Local(_))
            );
        let is_call = value.kind() == "call_expression";
        let property_name = match value.kind() {
            "simple_identifier" => Some(value),
            "navigation_expression" => property_name(value),
            _ => None,
        };
        // A stable property is narrowed as a local; one the compiler never
        // smart-casts keeps its declared type.
        let is_judged_property = property_name
            .and_then(|name| self.property_read_at(name))
            .is_some_and(|read| matches!(read, PropertyRead::Stable(_) | PropertyRead::Unstable));
        let unsure = self
            .narrowable(value)
            .is_some_and(|narrowed| self.is_unsure(narrowed));
        let followed = (is_local || is_call || is_judged_property)
            && !unsure
            && hir_def::def_map::classpath_complete(self.db, self.file)
            && self.examinable(value);
        if followed {
            NullKnowledge::Followed
        } else {
            NullKnowledge::Unfollowed
        }
    }

    /// A member of a class (not a top-level or extension declaration).
    fn is_own_member(&self, location: ItemLocation) -> bool {
        let tree = hir_def::item_tree(self.db, location.container);
        let item = tree.item(location.item);
        let is_extension = matches!(
            &item.signature,
            Signature::Function {
                receiver: Some(_),
                ..
            } | Signature::Property {
                receiver: Some(_),
                ..
            }
        );
        item.parent.is_some() && !is_extension
    }
}

/// Whether a chain of calls and properties has a safe call in it.
fn has_safe_step(node: SyntaxNode<'_>) -> bool {
    let mut current = node;
    loop {
        match current.kind() {
            "navigation_expression" => {
                let Some(suffix) = named_children(current).last() else {
                    return false;
                };
                if has_token(suffix, "?.") {
                    return true;
                }
                let Some(receiver) = named_children(current).next() else {
                    return false;
                };
                current = receiver;
            }
            "call_expression" | "parenthesized_expression" => {
                let Some(inner) = named_children(current).next() else {
                    return false;
                };
                current = inner;
            }
            _ => return false,
        }
    }
}
