//! Smart casts a called function's contract gives: `requireNotNull(x)`
//! makes `x` not null after it, `!s.isNullOrEmpty()` makes `s` not null
//! where it holds. The contract is the function's own — read from its
//! class file's metadata or its `contract { … }` — never assumed by name:
//! `assert(x != null)` has none (assertions may be off), so it casts
//! nothing.

use hir_def::ItemLocation;
use hir_def::item_tree::{
    Contract, ContractCondition, ContractSubject, Returns, Signature, TypeRef,
};
use syntax::{SyntaxNode, has_token, named_children};

use super::declarations::Inferrer;
use super::{LocalId, Reference};
use crate::lower::{TypeScope, lower_type};
use crate::ty::{Substitution, Ty, TypeParameterId};

/// A call of a function with a contract, its subjects found in the call.
pub(super) struct CalledContract<'tree> {
    contract: Contract,
    declaration: ItemLocation,
    /// `x` of `x.isNullOrEmpty()`.
    receiver: Option<SyntaxNode<'tree>>,
    /// Each parameter's argument, by the parameter's position.
    arguments: Vec<Option<SyntaxNode<'tree>>>,
    /// The call's written type arguments (`assertIs<X509Certificate>`), by
    /// the function's type parameters.
    type_arguments: Substitution,
}

impl<'tree> CalledContract<'tree> {
    /// The conditions holding when the call returned as `outcome` says.
    fn conditions(&self, holds: impl Fn(Returns) -> bool) -> Vec<&ContractCondition<TypeRef>> {
        self.contract
            .effects
            .iter()
            .filter(|effect| holds(effect.returns))
            .map(|effect| &effect.implies)
            .collect()
    }

    fn subject(&self, subject: ContractSubject) -> Option<SyntaxNode<'tree>> {
        match subject {
            ContractSubject::Receiver => self.receiver,
            ContractSubject::Parameter(index) => {
                self.arguments.get(index as usize).copied().flatten()
            }
        }
    }
}

impl Inferrer<'_> {
    /// The contract of what `call` (a `call_expression`, already inferred)
    /// calls, with its subjects; none for a safe call (`x?.f()`), whose
    /// value says nothing when `x` is null.
    pub(super) fn called_contract<'tree>(
        &self,
        call: SyntaxNode<'tree>,
    ) -> Option<CalledContract<'tree>> {
        if call.kind() != "call_expression" {
            return None;
        }
        let mut parts = named_children(call);
        let (callee, suffix) = (parts.next()?, parts.next()?);
        let (name, receiver) = match callee.kind() {
            "simple_identifier" => (callee, None),
            "navigation_expression" => {
                let navigation_suffix = named_children(callee).last()?;
                if has_token(navigation_suffix, "?.") {
                    return None;
                }
                let name = named_children(navigation_suffix)
                    .find(|child| child.kind() == "simple_identifier")?;
                (name, named_children(callee).next())
            }
            _ => return None,
        };
        let Reference::Declaration(declaration) = self.recorded_at(name)?.reference else {
            return None;
        };
        let tree = hir_def::item_tree(self.db, declaration.container);
        let contract = tree.contract(declaration.item)?.clone();
        let Signature::Function { parameters, .. } = &tree.item(declaration.item).signature else {
            return None;
        };
        let names: Vec<_> = parameters.iter().map(|parameter| parameter.name).collect();
        let arguments = arguments_by_parameter(self, suffix, &names);
        let mut type_arguments = Substitution::default();
        let written = named_children(suffix)
            .find(|child| child.kind() == "type_arguments")
            .into_iter()
            .flat_map(named_children)
            .filter_map(|projection| self.type_child(projection));
        for (index, written) in written.enumerate() {
            let parameter = TypeParameterId {
                owner: declaration,
                index: index as u32,
            };
            type_arguments.insert(parameter, self.written_type(written));
        }
        Some(CalledContract {
            contract,
            declaration,
            receiver,
            arguments,
            type_arguments,
        })
    }

    /// What the conditions holding when the call returned as `holds` says
    /// make the locals they're about.
    pub(super) fn contract_narrowings(
        &self,
        called: &CalledContract<'_>,
        holds: impl Fn(Returns) -> bool,
    ) -> Vec<(LocalId, Ty)> {
        called
            .conditions(holds)
            .into_iter()
            .flat_map(|condition| self.condition_narrowings(called, condition))
            .collect()
    }

    fn condition_narrowings(
        &self,
        called: &CalledContract<'_>,
        condition: &ContractCondition<TypeRef>,
    ) -> Vec<(LocalId, Ty)> {
        match condition {
            ContractCondition::IsNull {
                subject,
                negated: true,
            } => called
                .subject(*subject)
                .and_then(|node| self.asserted_not_null(node))
                .into_iter()
                .collect(),
            ContractCondition::IsInstance {
                subject,
                ty,
                negated: false,
            } => {
                let Some(local) = called
                    .subject(*subject)
                    .and_then(|node| self.local_named_by(node))
                else {
                    return Vec::new();
                };
                let scope = TypeScope::of_declaration(self.db, self.file, called.declaration);
                // `assertIs<X509Certificate>(x)`: `T` is the written type
                // argument; one the call infers isn't followed.
                let target = lower_type(self.db, scope, ty).substitute(&called.type_arguments);
                let generic = target.has_type_parameter() || target.is_unknown();
                if generic {
                    return Vec::new();
                }
                vec![(local, self.smart_cast(&self.local_type(local), target))]
            }
            // `require(x != null && y is T)`: the argument is the condition.
            ContractCondition::Holds {
                subject,
                negated: false,
            } => called
                .subject(*subject)
                .map(|node| self.asserted(node))
                .unwrap_or_default(),
            ContractCondition::All(parts) => parts
                .iter()
                .flat_map(|part| self.condition_narrowings(called, part))
                .collect(),
            // A null, a negated check, either of two: no one type follows.
            ContractCondition::IsNull { .. }
            | ContractCondition::IsInstance { .. }
            | ContractCondition::Holds { .. }
            | ContractCondition::Any(_) => Vec::new(),
        }
    }

    /// The local a plain name means.
    fn local_named_by(&self, node: SyntaxNode<'_>) -> Option<LocalId> {
        (node.kind() == "simple_identifier")
            .then(|| self.lookup_value_local(self.name(node)))
            .flatten()
    }
}

/// Each parameter's argument expression: positional ones in order, named
/// ones by name; a trailing lambda is the last parameter's.
fn arguments_by_parameter<'tree>(
    inferrer: &Inferrer<'_>,
    suffix: SyntaxNode<'tree>,
    names: &[intern::Name],
) -> Vec<Option<SyntaxNode<'tree>>> {
    let mut found = vec![None; names.len()];
    let Some(arguments) = named_children(suffix).find(|child| child.kind() == "value_arguments")
    else {
        return found;
    };
    let mut position = 0;
    for argument in named_children(arguments).filter(|child| child.kind() == "value_argument") {
        let children: Vec<SyntaxNode<'tree>> = named_children(argument).collect();
        let Some(&value) = children.last() else {
            continue;
        };
        let named = has_token(argument, "=") && children.len() >= 2;
        let index = if named {
            let name = inferrer.name(children[0]);
            names.iter().position(|parameter| *parameter == name)
        } else {
            let index = position;
            position += 1;
            Some(index)
        };
        if let Some(slot) = index.and_then(|index| found.get_mut(index)) {
            *slot = Some(value);
        }
    }
    found
}
