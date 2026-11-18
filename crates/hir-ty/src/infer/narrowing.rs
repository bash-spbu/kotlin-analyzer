//! Smart casts: what a condition tells about locals when it holds, and
//! when it doesn't (`x != null`, `x is String`, combined with `&&`, `||`,
//! `!`).

use hir_def::ItemLocation;
use hir_def::item_tree::Signature;
use syntax::ast::kotlin::TypeTest;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::Inferrer;
use super::member_scope::data_flow_scope;
use super::{LocalId, NullCheckKind, Reference, UnsafeCall};
use crate::ty::{ClassType, Nullability, Ty, TypeParameterId};

/// Members every value has, which Kotlin also offers on a nullable
/// receiver (`Any?.toString()`, `Any?.hashCode()`).
const NULL_SAFE_MEMBERS: &[&str] = &["equals", "hashCode", "toString"];

/// Calls that assert their argument isn't null, or a condition about it.
const ASSERTIONS: &[&str] = &[
    "require",
    "requireNotNull",
    "check",
    "checkNotNull",
    "assert",
    "assertNotNull",
    "assertThat",
];

/// Standard-library functions that throw unless their condition holds:
/// after `require(x != null)` the code sees `x` as not null.
const CONDITION_CONTRACTS: &[&str] = &["require", "check", "assert"];

/// Standard-library functions that throw when their argument is null.
const NOT_NULL_CONTRACTS: &[&str] = &["requireNotNull", "checkNotNull"];

/// An expression as data flow compares it: its text without whitespace,
/// without a leading `this.`, and with `?.` and `!!.` read as `.` —
/// `this.user`, `user`, and the `user?.name` of `user?.name != null` and
/// the `user.name` after it are one path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct StablePath(String);

impl StablePath {
    pub(super) fn of(text: &str) -> StablePath {
        let compact: String = text
            .chars()
            .filter(|char| !char.is_whitespace())
            .collect::<String>()
            .replace("?.", ".")
            .replace("!!.", ".");
        match compact.strip_prefix("this.") {
            Some(member) => StablePath(member.to_owned()),
            None => StablePath(compact),
        }
    }

    /// Whether `text` mentions the path anywhere (an assertion's argument).
    fn mentioned_in(&self, text: &str) -> bool {
        text.replace(char::is_whitespace, "").contains(&self.0)
    }
}

/// The locals a condition narrows, and to what.
#[derive(Default)]
pub(super) struct Narrowing {
    pub(super) when_true: Vec<(LocalId, Ty)>,
    pub(super) when_false: Vec<(LocalId, Ty)>,
    /// What `this is T` makes the innermost receiver when the condition
    /// holds.
    pub(super) receiver_when_true: Option<Ty>,
}

impl Narrowing {
    fn negated(self) -> Narrowing {
        Narrowing {
            when_true: self.when_false,
            when_false: self.when_true,
            receiver_when_true: None,
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
                self.push_scope();
                self.narrow(&left.when_true);
                let right = self.condition(right);
                self.pop_scope();
                Narrowing {
                    when_true: left.when_true.into_iter().chain(right.when_true).collect(),
                    when_false: Vec::new(),
                    receiver_when_true: right.receiver_when_true.or(left.receiver_when_true),
                }
            }
            "disjunction_expression" => {
                let mut operands = named_children(node);
                let (Some(left), Some(right)) = (operands.next(), operands.next()) else {
                    return Narrowing::default();
                };
                let left = self.condition(left);
                self.push_scope();
                self.narrow(&left.when_false);
                let right = self.condition(right);
                self.pop_scope();
                Narrowing {
                    when_true: Vec::new(),
                    when_false: left
                        .when_false
                        .into_iter()
                        .chain(right.when_false)
                        .collect(),
                    receiver_when_true: None,
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
            _ => {
                self.expression(node, None);
                Narrowing::default()
            }
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
            ("simple_identifier", "null_literal") => left,
            ("null_literal", "simple_identifier") => right,
            _ => return Narrowing::default(),
        };
        let Some(local) = self.lookup_value_local(self.name(checked)) else {
            return Narrowing::default();
        };
        let not_null = vec![(local, self.local_type(local).non_null())];
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
        if operand.kind() == "this_expression" && test == TypeTest::Is {
            return Narrowing {
                receiver_when_true: Some(target_type),
                ..Narrowing::default()
            };
        }
        let local = if operand.kind() == "simple_identifier" {
            self.lookup_value_local(self.name(operand))
        } else {
            None
        };
        let Some(local) = local else {
            return Narrowing::default();
        };
        let narrowed = vec![(local, self.smart_cast(&self.local_type(local), target_type))];
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
                .any(|argument| *argument != Ty::Unknown && !declared.arguments.contains(argument));
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
    pub(super) fn narrow_after_contract(&mut self, statement: SyntaxNode<'_>) {
        if statement.kind() != "call_expression" {
            return;
        }
        let mut children = named_children(statement);
        let (Some(callee), Some(suffix)) = (children.next(), children.next()) else {
            return;
        };
        if callee.kind() != "simple_identifier" || !self.resolves_into_kotlin(callee) {
            return;
        }
        let Some(argument) = first_argument(suffix) else {
            return;
        };
        let callee = self.text(callee);
        let narrowings = if CONDITION_CONTRACTS.contains(&callee) {
            self.asserted(argument)
        } else if NOT_NULL_CONTRACTS.contains(&callee) {
            self.asserted_not_null(argument).into_iter().collect()
        } else {
            Vec::new()
        };
        self.narrow(&narrowings);
    }

    /// Whether the name resolved to the `kotlin` package's declaration —
    /// not a namesake that may not throw.
    fn resolves_into_kotlin(&self, name: SyntaxNode<'_>) -> bool {
        let Some(resolved) = self.recorded_at(name) else {
            return false;
        };
        let Reference::Declaration(location) = resolved.reference else {
            return false;
        };
        hir_def::item_tree(self.db, location.container)
            .package
            .to_string()
            == "kotlin"
    }

    /// What a condition known to hold says, read without inferring it again
    /// (it already was, as an argument): `x != null`, `x is T`, joined by
    /// `&&`.
    fn asserted(&self, condition: SyntaxNode<'_>) -> Vec<(LocalId, Ty)> {
        match condition.kind() {
            "parenthesized_expression" => named_children(condition)
                .next()
                .map(|inner| self.asserted(inner))
                .unwrap_or_default(),
            "conjunction_expression" => named_children(condition)
                .flat_map(|operand| self.asserted(operand))
                .collect(),
            "equality_expression" if has_token(condition, "!=") => {
                let operands: Vec<SyntaxNode<'_>> = named_children(condition).collect();
                match operands[..] {
                    [checked, other] | [other, checked] if other.kind() == "null_literal" => {
                        self.asserted_not_null(checked).into_iter().collect()
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
                let local = (operand.kind() == "simple_identifier")
                    .then(|| self.lookup_value_local(self.name(operand)))
                    .flatten();
                match (local, target) {
                    (Some(local), Some(target)) => {
                        let target = self.written_type(target);
                        vec![(local, self.smart_cast(&self.local_type(local), target))]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// `x` known not to be null.
    pub(super) fn asserted_not_null(&self, node: SyntaxNode<'_>) -> Option<(LocalId, Ty)> {
        if node.kind() != "simple_identifier" {
            return None;
        }
        let local = self.lookup_value_local(self.name(node))?;
        Some((local, self.local_type(local).non_null()))
    }
}

/// A call suffix's first value argument's expression.
fn first_argument(suffix: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let arguments = named_children(suffix).find(|part| part.kind() == "value_arguments")?;
    let argument = named_children(arguments).find(|child| child.kind() == "value_argument")?;
    named_children(argument).last()
}

impl Inferrer<'_> {
    /// Records `receiver.member` (with a plain `.`) as unsafe when the
    /// receiver's type is nullable, the member is the class's own (not an
    /// extension, which may take a nullable receiver), and nothing in the
    /// declaration around could have smart-cast the receiver: it's never
    /// compared with `null`, type-checked, asserted, assigned or used with
    /// `?.`, `!!` or `?:` there.
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
        let path = StablePath::of(self.text(receiver));
        if self.null_guarded(receiver, &path) {
            return;
        }
        self.record_unsafe_call(UnsafeCall {
            range: TextRange::of(name),
            receiver: receiver_type.clone(),
        });
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

    /// Whether the expression spelled `path` is checked for `null` (or
    /// asserted, assigned, used null-safely) anywhere in the declaration
    /// around `node`.
    fn null_guarded(&self, node: SyntaxNode<'_>, path: &StablePath) -> bool {
        let scope = data_flow_scope(node);
        syntax::descendants(scope).any(|candidate| self.guards(candidate, path))
    }

    /// Whether `node` compares the expression spelled `path` with `null`,
    /// type-checks, asserts, assigns or uses it null-safely.
    pub(super) fn guards(&self, node: SyntaxNode<'_>, path: &StablePath) -> bool {
        let spelled = |child: Option<SyntaxNode<'_>>| {
            child.is_some_and(|child| StablePath::of(self.text(child)) == *path)
        };
        let mut children = named_children(node);
        match node.kind() {
            "equality_expression" => {
                let operands: Vec<SyntaxNode<'_>> = named_children(node).collect();
                let has_null = operands
                    .iter()
                    .any(|operand| operand.kind() == "null_literal");
                has_null && operands.iter().any(|operand| spelled(Some(*operand)))
            }
            "check_expression" | "postfix_expression" | "elvis_expression" | "when_subject"
            | "assignment" => spelled(children.next()),
            "directly_assignable_expression" => StablePath::of(self.text(node)) == *path,
            "navigation_expression" => {
                let receiver = children.next();
                let suffix = children.next();
                let null_safe = suffix.is_some_and(|suffix| {
                    has_token(suffix, "?.")
                        || named_children(suffix).any(|name| {
                            self.text(name).starts_with("isNull") || self.text(name) == "orEmpty"
                        })
                });
                null_safe && spelled(receiver)
            }
            "call_expression" => {
                let callee = children
                    .next()
                    .map(|callee| self.text(callee))
                    .unwrap_or("");
                let asserts = ASSERTIONS.contains(&callee) || callee.starts_with("assert");
                asserts && path.mentioned_in(self.text(node))
            }
            "variable_declaration" => {
                // `var x: T? = value`: what was assigned may have been
                // non-null.
                named_children(node)
                    .next()
                    .is_some_and(|name| StablePath::of(self.text(name)) == *path)
            }
            _ => false,
        }
    }
}
