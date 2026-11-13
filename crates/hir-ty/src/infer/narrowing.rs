//! Smart casts: what a condition tells about locals when it holds, and
//! when it doesn't (`x != null`, `x is String`, combined with `&&`, `||`,
//! `!`).

use hir_def::ItemLocation;
use hir_def::item_tree::Signature;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::Inferrer;
use super::{LocalId, UnsafeCall};
use crate::ty::{Nullability, Ty};

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
        for operand in &operands {
            self.expression(*operand, None);
        }
        let [left, right] = operands[..] else {
            return Narrowing::default();
        };
        let checked = match (left.kind(), right.kind()) {
            ("simple_identifier", "null_literal") => left,
            ("null_literal", "simple_identifier") => right,
            _ => return Narrowing::default(),
        };
        let Some(local) = self.lookup_local(self.name(checked)) else {
            return Narrowing::default();
        };
        let not_null = vec![(local, self.local_type(local).non_null())];
        if has_token(node, "!=") || has_token(node, "!==") {
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
        self.expression(operand, None);
        let is_type_check = has_token(node, "is") || has_token(node, "!is");
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
        let target_type = self.written_type(*target);
        if operand.kind() == "this_expression" && !has_token(node, "!is") {
            return Narrowing {
                receiver_when_true: Some(target_type),
                ..Narrowing::default()
            };
        }
        let local = if operand.kind() == "simple_identifier" {
            self.lookup_local(self.name(operand))
        } else {
            None
        };
        let Some(local) = local else {
            return Narrowing::default();
        };
        let narrowed = vec![(local, target_type)];
        if has_token(node, "!is") {
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
        let path = stable_path(self.text(receiver));
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
    fn null_guarded(&self, node: SyntaxNode<'_>, path: &str) -> bool {
        let scope = enclosing_declaration(node);
        syntax::descendants(scope).any(|candidate| self.guards(candidate, path))
    }

    fn guards(&self, node: SyntaxNode<'_>, path: &str) -> bool {
        let spelled = |child: Option<SyntaxNode<'_>>| {
            child.is_some_and(|child| stable_path(self.text(child)) == path)
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
            "directly_assignable_expression" => stable_path(self.text(node)) == path,
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
                asserts
                    && self
                        .text(node)
                        .replace(char::is_whitespace, "")
                        .contains(path)
            }
            "variable_declaration" => {
                // `var x: T? = value`: what was assigned may have been
                // non-null.
                named_children(node)
                    .next()
                    .is_some_and(|name| self.text(name) == path)
            }
            _ => false,
        }
    }
}

/// An expression's text without whitespace, to compare paths like
/// `this.user` and `account.owner.name`.
fn stable_path(text: &str) -> String {
    text.chars().filter(|char| !char.is_whitespace()).collect()
}

/// The member-level declaration around `node` (a function, an accessor,
/// an initializer, a property), or the file.
fn enclosing_declaration(node: SyntaxNode<'_>) -> SyntaxNode<'_> {
    let mut current = node;
    let mut found = node;
    while let Some(parent) = current.parent() {
        let is_declaration = matches!(
            parent.kind(),
            "function_declaration"
                | "getter"
                | "setter"
                | "anonymous_initializer"
                | "secondary_constructor"
                | "property_declaration"
        );
        let at_member_level = parent.parent().is_some_and(|grandparent| {
            matches!(
                grandparent.kind(),
                "class_body" | "source_file" | "enum_class_body"
            )
        });
        if is_declaration && at_member_level {
            return parent;
        }
        found = parent;
        current = parent;
    }
    found
}
