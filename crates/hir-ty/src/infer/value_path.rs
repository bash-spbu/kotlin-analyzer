//! Values as the data-flow checks after inference name them: a local, or
//! a chain of properties read from a local, from `this` (written or
//! implicit) or from an object — by what each name resolved to, not by how
//! it's spelled. `user`, `this.user`, `(user)`, `user!!` and the `user` of
//! `user?.name` are one value; `user.name` and `user?.name` another; a
//! call's result is none (the compiler never narrows one).

use hir_def::ItemLocation;
use hir_def::item_tree::ItemKind;
use syntax::{SyntaxNode, has_token, named_children};

use super::body_facts::BodyFacts;
use super::member_scope::data_flow_scope;
use super::{LocalId, Reference};

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct ValuePath {
    root: PathStart,
    /// The properties read from the root, outermost first.
    properties: Vec<ItemLocation>,
}

/// What a value path reads from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PathStart {
    Local(LocalId),
    /// `this` without a label, or no receiver written: a property read
    /// from the implicit receiver (or a top-level one).
    This,
    /// An object, or a class whose statics are read.
    Classifier(ItemLocation),
}

impl ValuePath {
    fn local(local: LocalId) -> ValuePath {
        ValuePath {
            root: PathStart::Local(local),
            properties: Vec::new(),
        }
    }

    fn read(mut self, property: ItemLocation) -> ValuePath {
        self.properties.push(property);
        self
    }
}

impl BodyFacts<'_, '_> {
    /// The value `node` names, if it names one.
    pub(super) fn value_path(&self, node: SyntaxNode<'_>) -> Option<ValuePath> {
        match node.kind() {
            "parenthesized_expression" => self.value_path(named_children(node).next()?),
            "postfix_expression" if has_token(node, "!!") => {
                self.value_path(named_children(node).next()?)
            }
            "this_expression" => {
                let labelled = named_children(node).next().is_some();
                (!labelled).then(|| ValuePath {
                    root: PathStart::This,
                    properties: Vec::new(),
                })
            }
            "simple_identifier" => self.name_path(node),
            "navigation_expression" | "directly_assignable_expression" => {
                let mut parts = named_children(node);
                let first = parts.next()?;
                let Some(suffix) = parts.next() else {
                    // `x = …`: the assigned name alone.
                    return self.value_path(first);
                };
                let name =
                    named_children(suffix).find(|part| part.kind() == "simple_identifier")?;
                let Reference::Declaration(property) = self.recorded_at(name)?.reference else {
                    return None;
                };
                Some(self.value_path(first)?.read(property))
            }
            _ => None,
        }
    }

    /// The local `name` declares (`var x: T? = …`), as a value path.
    pub(super) fn declared_path(&self, name: SyntaxNode<'_>) -> Option<ValuePath> {
        let local = self.result.local_declared_at(name.start_byte() as u32)?;
        Some(ValuePath::local(local))
    }

    fn name_path(&self, name: SyntaxNode<'_>) -> Option<ValuePath> {
        match self.recorded_at(name)?.reference {
            Reference::Local(local) => Some(ValuePath::local(local)),
            Reference::Declaration(location) => {
                let tree = hir_def::item_tree(self.db, location.container);
                let root = match tree.item(location.item).kind {
                    ItemKind::Property { .. } => {
                        return Some(
                            ValuePath {
                                root: PathStart::This,
                                properties: Vec::new(),
                            }
                            .read(location),
                        );
                    }
                    ItemKind::Class(_) | ItemKind::TypeAlias => PathStart::Classifier(location),
                    _ => return None,
                };
                Some(ValuePath {
                    root,
                    properties: Vec::new(),
                })
            }
            Reference::Candidates(_) => None,
        }
    }
}

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

impl BodyFacts<'_, '_> {
    /// Whether `path` is checked for `null` (or asserted, assigned, used
    /// null-safely) anywhere in the declaration around `node`: where the
    /// data flow doesn't follow a value, what the compiler may have
    /// narrowed it by.
    pub(super) fn null_guarded(&self, node: SyntaxNode<'_>, path: &ValuePath) -> bool {
        let scope = data_flow_scope(node);
        syntax::descendants(scope).any(|candidate| self.guards(candidate, path))
    }

    /// Whether `node` compares `path` with `null`, type-checks, asserts,
    /// assigns or uses it null-safely.
    pub(super) fn guards(&self, node: SyntaxNode<'_>, path: &ValuePath) -> bool {
        let names_path = |child: Option<SyntaxNode<'_>>| {
            child.and_then(|child| self.value_path(child)).as_ref() == Some(path)
        };
        let mut children = named_children(node);
        match node.kind() {
            "equality_expression" => {
                let operands: Vec<SyntaxNode<'_>> = named_children(node).collect();
                let has_null = operands
                    .iter()
                    .any(|operand| operand.kind() == "null_literal");
                has_null && operands.iter().any(|operand| names_path(Some(*operand)))
            }
            "check_expression" | "postfix_expression" | "elvis_expression" | "when_subject"
            | "assignment" => names_path(children.next()),
            "directly_assignable_expression" => names_path(Some(node)),
            "navigation_expression" => {
                let receiver = children.next();
                let suffix = children.next();
                let null_safe = suffix.is_some_and(|suffix| {
                    has_token(suffix, "?.")
                        || named_children(suffix).any(|name| {
                            self.text(name).starts_with("isNull") || self.text(name) == "orEmpty"
                        })
                });
                null_safe && names_path(receiver)
            }
            "call_expression" => {
                let callee = children
                    .next()
                    .map(|callee| self.text(callee))
                    .unwrap_or("");
                let asserts = ASSERTIONS.contains(&callee) || callee.starts_with("assert");
                // Whatever the assertion mentions: `requireNotNull(user)`,
                // `check(user.name != null)`.
                asserts
                    && syntax::descendants(node)
                        .any(|part| self.value_path(part).as_ref() == Some(path))
            }
            // `var x: T? = value`: what was assigned may have been non-null.
            "variable_declaration" => {
                named_children(node)
                    .next()
                    .and_then(|name| self.declared_path(name))
                    .as_ref()
                    == Some(path)
            }
            _ => false,
        }
    }
}
