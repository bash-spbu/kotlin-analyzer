//! The grammar binds `a + b.f<T>()?.g` as `(a + b.f)<T>()?.g`: a postfix
//! chain whose base is a whole operation. In Kotlin every postfix binds
//! tighter than any binary operator (and a real `(a + b).f` keeps its
//! parentheses), so such a chain belongs to the operation's last operand:
//! `a + (b.f<T>()?.g)`. Inference reads the tree that way.

use syntax::{SyntaxNode, has_token, named_children};

/// An operation the grammar may wrongly put at the base of a postfix chain,
/// and how its value follows from its operands once re-associated.
#[derive(Clone, Copy)]
pub(super) enum Operation {
    /// `+ - * / % .. ..<`: the left operand's operator function.
    Operator(&'static str),
    /// `== != < > <= >=`.
    Comparison,
    /// `a ?: chain`.
    Elvis,
}

impl Operation {
    pub(super) fn of(node: SyntaxNode<'_>) -> Option<Operation> {
        match node.kind() {
            "additive_expression" | "multiplicative_expression" | "range_expression" => {
                operator_function(node).map(Operation::Operator)
            }
            "comparison_expression" | "equality_expression" => Some(Operation::Comparison),
            "elvis_expression" => Some(Operation::Elvis),
            _ => None,
        }
    }
}

/// The operator function a `+`, `*`, `..` and the like calls.
pub(super) fn operator_function(node: SyntaxNode<'_>) -> Option<&'static str> {
    [
        ("+", "plus"),
        ("-", "minus"),
        ("*", "times"),
        ("/", "div"),
        ("%", "rem"),
        ("..<", "rangeUntil"),
        ("..", "rangeTo"),
    ]
    .into_iter()
    .find(|(token, _)| has_token(node, token))
    .map(|(_, function)| function)
}

/// A misparsed chain's top: the operation at its base, its leading operand,
/// and so the chain's real value is `leading <operation> chain`.
pub(super) struct MisparsedChain<'tree> {
    pub(super) operation: Operation,
    pub(super) leading: SyntaxNode<'tree>,
}

fn is_postfix(node: SyntaxNode<'_>) -> bool {
    matches!(
        node.kind(),
        "navigation_expression" | "call_expression" | "indexing_expression" | "postfix_expression"
    )
}

/// `node` continues the postfix chain of its parent's base: the grammar
/// put it where only a postfix's receiver goes. A cast or type check there
/// (`(x is A).B` for `x is A.B`) lost the rest of its type.
pub(super) fn continues_chain(node: SyntaxNode<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| is_postfix(parent) && named_children(parent).next() == Some(node))
}

/// When `node` is the top of a postfix chain based on an operation, that
/// operation's leading operand and kind.
pub(super) fn misparsed_chain(node: SyntaxNode<'_>) -> Option<MisparsedChain<'_>> {
    if !is_postfix(node) || continues_chain(node) {
        return None;
    }
    let mut base = node;
    while is_postfix(base) {
        base = named_children(base).next()?;
    }
    let operation = Operation::of(base)?;
    let leading = named_children(base).next()?;
    Some(MisparsedChain { operation, leading })
}

/// The operand that really starts the chain, when `node` is an operation
/// the grammar put at a chain's base.
pub(super) fn chain_operand(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    if !continues_chain(node) {
        return None;
    }
    Operation::of(node)?;
    named_children(node).nth(1)
}
