//! A function's `contract { … }` as written: the first statement of its
//! body, its `returns… implies condition` effects (the compiler reads
//! nothing else for types). Names in a condition are the function's
//! parameters, `this` its receiver.

use syntax::{Parse, SyntaxNode, has_token, named_children};

use super::{Contract, ContractCondition, ContractEffect, ContractSubject, Returns, TypeRef};

/// The contract `function` (a `function_declaration`) writes, if any.
pub(super) fn written_contract(parse: &Parse, function: SyntaxNode<'_>) -> Option<Contract> {
    let body = named_children(function).find(|child| child.kind() == "function_body")?;
    let statements = named_children(body).find(|child| child.kind() == "statements")?;
    let first = named_children(statements).next()?;
    if first.kind() != "call_expression" {
        return None;
    }
    let mut parts = named_children(first);
    let callee = parts.next()?;
    if parse.text_of(callee) != "contract" {
        return None;
    }
    let lambda = syntax::descendants(parts.next()?).find(|node| node.kind() == "lambda_literal")?;
    let effects_block = named_children(lambda).find(|child| child.kind() == "statements")?;
    let parameters = parameter_names(parse, function);
    let reader = ConditionReader {
        parse,
        parameters: &parameters,
    };
    let effects: Box<[ContractEffect<TypeRef>]> = named_children(effects_block)
        .filter_map(|statement| reader.effect(statement))
        .collect();
    (!effects.is_empty()).then_some(Contract { effects })
}

fn parameter_names<'parse>(parse: &'parse Parse, function: SyntaxNode<'_>) -> Vec<&'parse str> {
    named_children(function)
        .filter(|child| child.kind() == "function_value_parameters")
        .flat_map(named_children)
        .filter(|parameter| parameter.kind() == "parameter")
        .filter_map(|parameter| {
            named_children(parameter).find(|child| child.kind() == "simple_identifier")
        })
        .map(|name| parse.text_of(name).trim_matches('`'))
        .collect()
}

struct ConditionReader<'reader> {
    parse: &'reader Parse,
    parameters: &'reader [&'reader str],
}

impl ConditionReader<'_> {
    /// `returns(…) implies condition`, `returnsNotNull() implies condition`.
    fn effect(&self, statement: SyntaxNode<'_>) -> Option<ContractEffect<TypeRef>> {
        if statement.kind() != "infix_expression" {
            return None;
        }
        let parts: Vec<SyntaxNode<'_>> = named_children(statement).collect();
        let [call, function, condition] = parts[..] else {
            return None;
        };
        if self.parse.text_of(function) != "implies" || call.kind() != "call_expression" {
            return None;
        }
        let mut call_parts = named_children(call);
        let callee = self.parse.text_of(call_parts.next()?);
        let argument = call_parts
            .next()
            .and_then(|suffix| {
                named_children(suffix).find(|child| child.kind() == "value_arguments")
            })
            .and_then(|arguments| named_children(arguments).next())
            .and_then(|argument| named_children(argument).last());
        let returns = match (callee, argument.map(|value| self.parse.text_of(value))) {
            ("returns", None) => Returns::Normally,
            ("returns", Some("true")) => Returns::Value(true),
            ("returns", Some("false")) => Returns::Value(false),
            ("returns", Some("null")) => Returns::Null,
            ("returnsNotNull", None) => Returns::NotNull,
            _ => return None,
        };
        let implies = self.condition(condition)?;
        Some(ContractEffect { returns, implies })
    }

    fn condition(&self, node: SyntaxNode<'_>) -> Option<ContractCondition<TypeRef>> {
        match node.kind() {
            "parenthesized_expression" => self.condition(named_children(node).next()?),
            "conjunction_expression" => Some(ContractCondition::All(
                named_children(node)
                    .map(|operand| self.condition(operand))
                    .collect::<Option<_>>()?,
            )),
            "disjunction_expression" => Some(ContractCondition::Any(
                named_children(node)
                    .map(|operand| self.condition(operand))
                    .collect::<Option<_>>()?,
            )),
            "prefix_expression" if has_token(node, "!") => {
                negated(self.condition(named_children(node).last()?)?)
            }
            "equality_expression" => {
                let operands: Vec<SyntaxNode<'_>> = named_children(node).collect();
                let subject = match operands[..] {
                    [subject, other] | [other, subject] if other.kind() == "null_literal" => {
                        self.subject(subject)?
                    }
                    _ => return None,
                };
                let negated = has_token(node, "!=") || has_token(node, "!==");
                Some(ContractCondition::IsNull { subject, negated })
            }
            "check_expression" => {
                let mut parts = named_children(node);
                let subject = self.subject(parts.next()?)?;
                let target = parts.find(|part| syntax::ast::kotlin::is_type(*part))?;
                let negated = has_token(node, "!is");
                Some(ContractCondition::IsInstance {
                    subject,
                    ty: super::kotlin_type_ref(self.parse, target),
                    negated,
                })
            }
            "simple_identifier" => Some(ContractCondition::Holds {
                subject: self.subject(node)?,
                negated: false,
            }),
            _ => None,
        }
    }

    /// A parameter by its name, or `this` (the receiver).
    fn subject(&self, node: SyntaxNode<'_>) -> Option<ContractSubject> {
        match node.kind() {
            "this_expression" => Some(ContractSubject::Receiver),
            "simple_identifier" => {
                let name = self.parse.text_of(node).trim_matches('`');
                let position = self
                    .parameters
                    .iter()
                    .position(|parameter| *parameter == name)?;
                Some(ContractSubject::Parameter(position as u32))
            }
            _ => None,
        }
    }
}

fn negated(condition: ContractCondition<TypeRef>) -> Option<ContractCondition<TypeRef>> {
    match condition {
        ContractCondition::IsNull { subject, negated } => Some(ContractCondition::IsNull {
            subject,
            negated: !negated,
        }),
        ContractCondition::IsInstance {
            subject,
            ty,
            negated,
        } => Some(ContractCondition::IsInstance {
            subject,
            ty,
            negated: !negated,
        }),
        ContractCondition::Holds { subject, negated } => Some(ContractCondition::Holds {
            subject,
            negated: !negated,
        }),
        ContractCondition::All(_) | ContractCondition::Any(_) => None,
    }
}
