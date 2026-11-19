//! The checks that need only what inference found, run once it's done —
//! on the body's syntax, the names it resolved and the facts it left
//! (`Pending`), never on the walk's state: a `when` that covers too little
//! or too much, a `val` assigned again, a nullable receiver used where the
//! data flow doesn't follow it and nothing around checks it. Each runs
//! once per node, however often inference walked it (a lambda inferred
//! against several candidates).

use rustc_hash::FxHashMap;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::body_facts::BodyFacts;
use super::exhaustiveness::Completeness;
use super::when_branches::redundant_else;
use super::{LocalKind, NonExhaustiveWhen, Reference, UnsafeCall, WhenFinding};
use crate::ty::Ty;

/// What inference leaves for the checks after it.
#[derive(Default)]
pub(super) struct Pending {
    /// Each `when`'s subject type (after smart casts), by the `when`'s
    /// start.
    pub(super) when_subjects: FxHashMap<u32, Ty>,
    /// `receiver.member` on a nullable receiver the data flow doesn't
    /// follow, by the member name's start: unsafe unless something around
    /// checks the receiver.
    pub(super) unfollowed_receivers: FxHashMap<u32, UnfollowedReceiver>,
}

pub(super) struct UnfollowedReceiver {
    pub(super) receiver: NodeSpan,
    pub(super) call: UnsafeCall,
}

/// Where a node is, to find it again in the tree.
#[derive(Clone, Copy)]
pub(super) struct NodeSpan {
    range: TextRange,
    kind: &'static str,
}

impl NodeSpan {
    pub(super) fn of(node: SyntaxNode<'_>) -> NodeSpan {
        NodeSpan {
            range: TextRange::of(node),
            kind: node.kind(),
        }
    }

    fn find<'tree>(self, root: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        let mut node =
            root.descendant_for_byte_range(self.range.start as usize, self.range.end as usize)?;
        loop {
            let spans = TextRange::of(node) == self.range;
            if spans && node.kind() == self.kind {
                return Some(node);
            }
            let parent = node.parent()?;
            if TextRange::of(parent) != self.range {
                return None;
            }
            node = parent;
        }
    }
}

/// What the checks after inference found.
#[derive(Default)]
pub(super) struct Findings {
    pub(super) non_exhaustive_whens: Vec<NonExhaustiveWhen>,
    pub(super) when_findings: Vec<WhenFinding>,
    pub(super) val_reassignments: Vec<TextRange>,
    pub(super) unsafe_calls: Vec<UnsafeCall>,
}

pub(super) fn validate(facts: &BodyFacts<'_, '_>, pending: &Pending) -> Findings {
    let mut findings = Findings::default();
    let root = facts.parse.root();
    for node in syntax::descendants(root) {
        match node.kind() {
            "when_expression" => {
                if let Some(subject_type) = pending.when_subjects.get(&(node.start_byte() as u32)) {
                    when_with_subject(facts, node, subject_type, &mut findings);
                }
            }
            "assignment" if has_token(node, "=") => {
                let target = named_children(node).next();
                findings
                    .val_reassignments
                    .extend(target.and_then(|target| reassigned(facts, target)));
            }
            "prefix_expression" | "postfix_expression" => {
                let increments = has_token(node, "++") || has_token(node, "--");
                let operand = match node.kind() {
                    "prefix_expression" => named_children(node).last(),
                    _ => named_children(node).next(),
                };
                if increments {
                    findings
                        .val_reassignments
                        .extend(operand.and_then(|operand| reassigned(facts, operand)));
                }
            }
            _ => {}
        }
    }
    let mut unfollowed: Vec<&UnfollowedReceiver> = pending.unfollowed_receivers.values().collect();
    unfollowed.sort_by_key(|unfollowed| unfollowed.call.range.start);
    for unfollowed in unfollowed {
        if unguarded(facts, root, unfollowed) {
            findings.unsafe_calls.push(unfollowed.call.clone());
        }
    }
    findings
}

/// A `when` with a subject: the cases it misses without `else`, its
/// `else` when the branches cover every case, conditions it repeats.
fn when_with_subject(
    facts: &BodyFacts<'_, '_>,
    node: SyntaxNode<'_>,
    subject_type: &Ty,
    findings: &mut Findings,
) {
    let has_else = named_children(node).any(|entry| {
        entry.kind() == "when_entry"
            && !named_children(entry).any(|part| part.kind() == "when_condition")
    });
    if !has_else {
        let missing = facts.missing_cases(node, subject_type, Completeness::Found);
        if let Some(missing) = missing.filter(|missing| !missing.is_empty()) {
            let start = node.start_byte();
            findings.non_exhaustive_whens.push(NonExhaustiveWhen {
                range: TextRange::new(start, start + "when".len()),
                missing,
            });
        }
    }
    findings.when_findings.extend(
        facts
            .duplicate_conditions(node)
            .into_iter()
            .map(|range| WhenFinding::DuplicateCondition { range }),
    );
    if has_else && facts.covers_all_cases(node, subject_type) {
        findings.when_findings.extend(redundant_else(node));
    }
}

/// `x = …`, `x++`: the name of `x` when it's a local that can't be
/// assigned again — a `val` that has a value (one declared without may get
/// its first), a parameter, a loop's or a lambda's variable.
fn reassigned(facts: &BodyFacts<'_, '_>, target: SyntaxNode<'_>) -> Option<TextRange> {
    let name = match target.kind() {
        "directly_assignable_expression" => {
            let mut parts = named_children(target);
            match (parts.next(), parts.next()) {
                (Some(only), None) if only.kind() == "simple_identifier" => only,
                _ => return None,
            }
        }
        "simple_identifier" => target,
        _ => return None,
    };
    let Reference::Local(local) = facts.recorded_at(name)?.reference else {
        return None;
    };
    let declared = facts.result.local(local);
    let has_value = match declared.kind {
        LocalKind::Value => !declared_without_value(facts, declared.name_range),
        LocalKind::Parameter | LocalKind::LambdaParameter | LocalKind::ImplicitIt => true,
        _ => false,
    };
    has_value.then(|| TextRange::of(name))
}

/// Whether the `val` named at `name` is declared without a value (`val x:
/// T` given one later): neither an initializer nor a delegate.
fn declared_without_value(facts: &BodyFacts<'_, '_>, name: TextRange) -> bool {
    let root = facts.parse.root();
    let property = root
        .descendant_for_byte_range(name.start as usize, name.end as usize)
        .and_then(|name| name.parent())
        .filter(|declaration| declaration.kind() == "variable_declaration")
        .and_then(|declaration| declaration.parent())
        .filter(|property| property.kind() == "property_declaration");
    let Some(property) = property else {
        return false;
    };
    let delegated = named_children(property).any(|part| part.kind() == "property_delegate");
    super::declarations::initializer_of(property).is_none() && !delegated
}

/// Whether nothing around the receiver could have smart-cast it: no value
/// path names it (a call's result is never narrowed), or none of the
/// declaration's checks is of its path.
fn unguarded(
    facts: &BodyFacts<'_, '_>,
    root: SyntaxNode<'_>,
    unfollowed: &UnfollowedReceiver,
) -> bool {
    let Some(receiver) = unfollowed.receiver.find(root) else {
        return false;
    };
    match facts.value_path(receiver) {
        Some(path) => !facts.null_guarded(receiver, &path),
        None => true,
    }
}
