//! Smart casts: what a condition tells about locals when it holds, and
//! when it doesn't (`x != null`, `x is String`, combined with `&&`, `||`,
//! `!`).

use syntax::{SyntaxNode, has_token, named_children};

use super::LocalId;
use super::declarations::Inferrer;
use crate::ty::Ty;

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
