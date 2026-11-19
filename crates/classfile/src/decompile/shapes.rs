//! What javac compiled source shapes into, read back as those shapes:
//!
//! - `int i = 0; while (i < n) { …; i++; }` — a `for`;
//! - a `for` over a hidden index of a hidden array copy, its element
//!   taken first — a `for`-each over the array;
//! - a hidden iterator's `while (it.hasNext())`, its element taken first —
//!   a `for`-each over the `Iterable`;
//! - a `switch` on a hidden copy's `hashCode()` choosing an index by
//!   `equals`, then a `switch` on the index — a `switch` on strings.
//!
//! The hidden variables are javac's: no name in the local variable table
//! (so `varN`), used nowhere else.

use super::structure::{children, continues, expression_mentions, mentions};
use super::{Expression, Literal, LoopKind, Receiver, Relation, Statement, SwitchCase};

pub(super) fn form_source_shapes(statements: &mut Vec<Statement>) {
    for statement in statements.iter_mut() {
        for child in children(statement) {
            form_source_shapes(child);
        }
    }
    let mut index = 0;
    while index < statements.len() {
        form_for(statements, index);
        index += 1;
    }
    let mut index = 0;
    while index < statements.len() {
        let collapsed = form_for_each_over_array(statements, index)
            || form_for_each_over_iterable(statements, index)
            || form_string_switch(statements, index);
        if collapsed {
            index = index.saturating_sub(2);
        } else {
            index += 1;
        }
    }
}

/// The variable a statement updates: `i++`, `i = i + 2`.
fn updated_variable(statement: &Statement) -> Option<&str> {
    match statement {
        Statement::Increment { name, .. } => Some(name),
        Statement::Assign {
            target: Expression::Local(name),
            ..
        } => Some(name),
        _ => None,
    }
}

/// Whether nothing in `statements` mentions the variable `name`.
fn unmentioned(statements: &[Statement], name: &str) -> bool {
    !statements.iter().any(|statement| mentions(statement, name))
}

/// `while (c) { …; update; }` with what came before setting the variable
/// `c` tests: a `for`.
fn form_for(statements: &mut Vec<Statement>, index: usize) {
    let Statement::Loop {
        label,
        kind: LoopKind::While(condition),
        body,
    } = &mut statements[index]
    else {
        return;
    };
    let label = *label;
    let updates_tested = |statement: &Statement| {
        updated_variable(statement).is_some_and(|name| expression_mentions(condition, name))
    };
    // `{ … continue → break inner; … } update`: a `continue` runs the
    // update, as in a `for`.
    let wrapped = match body.as_slice() {
        [Statement::Block { label: inner, .. }, update] if updates_tested(update) => Some(*inner),
        _ => None,
    };
    let update = if let Some(inner_label) = wrapped {
        let Some(update) = body.pop() else {
            return;
        };
        let Some(Statement::Block { body: inner, .. }) = body.pop() else {
            return;
        };
        let continuing = match label {
            Some(label) => Statement::Continue(Some(label)),
            None => Statement::Continue(None),
        };
        *body = inner;
        replace_breaks_everywhere(body, inner_label, &continuing);
        update
    } else {
        let ends_in_update = body.last().is_some_and(updates_tested);
        if !ends_in_update || continues(body, label) {
            return;
        }
        let Some(update) = body.pop() else {
            return;
        };
        update
    };
    let condition = condition.clone();
    let variable = updated_variable(&update).unwrap_or_default().to_owned();
    let previous_sets_it = index > 0
        && match &statements[index - 1] {
            Statement::Declare {
                name,
                value: Some(_),
                ..
            } => *name == variable,
            Statement::Assign {
                target: Expression::Local(name),
                ..
            } => *name == variable,
            _ => false,
        };
    let declared_before = matches!(
        &statements[index - usize::from(index > 0)],
        Statement::Declare { .. }
    );
    let used_after = !unmentioned(&statements[index + 1..], &variable);
    let takes_initializer = previous_sets_it && !(declared_before && used_after);
    let initializer = if takes_initializer {
        Some(Box::new(statements.remove(index - 1)))
    } else {
        None
    };
    let at = if takes_initializer { index - 1 } else { index };
    if let Statement::Loop { kind, .. } = &mut statements[at] {
        *kind = LoopKind::For {
            initializer,
            condition,
            update: Box::new(update),
        };
    }
}

/// `break label` anywhere in `statements` as `replacement`.
fn replace_breaks_everywhere(
    statements: &mut [Statement],
    label: super::Label,
    replacement: &Statement,
) {
    for statement in statements.iter_mut() {
        if *statement == Statement::Break(Some(label)) {
            *statement = replacement.clone();
            continue;
        }
        for child in children(statement) {
            replace_breaks_everywhere(child, label, replacement);
        }
    }
}

/// `var a = E; var n = a.length; for (var i = 0; i < n; i++) { T x = a[i];
/// … }` is `for (T x : E) { … }`.
fn form_for_each_over_array(statements: &mut Vec<Statement>, index: usize) -> bool {
    if index < 2 {
        return false;
    }
    let (before, rest) = statements.split_at(index);
    let [.., array_declaration, length_declaration] = before else {
        return false;
    };
    let Statement::Declare {
        name: array,
        value: Some(iterable),
        ..
    } = array_declaration
    else {
        return false;
    };
    let Statement::Declare {
        name: length,
        value: Some(Expression::ArrayLength(length_of)),
        ..
    } = length_declaration
    else {
        return false;
    };
    let Some(Statement::Loop {
        label,
        kind:
            LoopKind::For {
                initializer: Some(initializer),
                condition,
                update,
            },
        body,
    }) = rest.first()
    else {
        return false;
    };
    let Statement::Declare {
        name: position,
        value: Some(Expression::Literal(Literal::Int(0))),
        ..
    } = initializer.as_ref()
    else {
        return false;
    };
    let counts_up = **update
        == Statement::Increment {
            name: position.clone(),
            by: 1,
        };
    let tests_length = *condition
        == Expression::Compare {
            relation: Relation::Less,
            left: Box::new(Expression::Local(position.clone())),
            right: Box::new(Expression::Local(length.clone())),
            floating: false,
        };
    let Some(Statement::Declare {
        type_ref,
        name: element,
        value:
            Some(Expression::ArrayElement {
                array: element_array,
                index: element_index,
            }),
    }) = body.first()
    else {
        return false;
    };
    let reads_the_array = **length_of == Expression::Local(array.clone())
        && **element_array == Expression::Local(array.clone())
        && **element_index == Expression::Local(position.clone());
    let hidden = [array, length, position].iter().all(|name| {
        name.starts_with("var") && unmentioned(&body[1..], name) && unmentioned(&rest[1..], name)
    });
    if !(counts_up && tests_length && reads_the_array && hidden) {
        return false;
    }
    let looped = Statement::Loop {
        label: *label,
        kind: LoopKind::ForEach {
            type_ref: type_ref.clone(),
            name: element.clone(),
            iterable: iterable.clone(),
        },
        body: body[1..].to_vec(),
    };
    statements.splice(index - 2..=index, [looped]);
    true
}

/// `var it = E.iterator(); while (it.hasNext()) { T x = (T) it.next(); … }`
/// is `for (T x : E) { … }`.
fn form_for_each_over_iterable(statements: &mut Vec<Statement>, index: usize) -> bool {
    if index < 1 {
        return false;
    }
    let Statement::Declare {
        name: iterator,
        value:
            Some(Expression::Call {
                receiver: Receiver::Instance(iterable),
                name: iterator_method,
                arguments: iterator_arguments,
            }),
        ..
    } = &statements[index - 1]
    else {
        return false;
    };
    let Statement::Loop {
        label,
        kind: LoopKind::While(condition),
        body,
    } = &statements[index]
    else {
        return false;
    };
    let call_on_iterator = |expression: &Expression, method: &str| {
        matches!(
            expression,
            Expression::Call { receiver: Receiver::Instance(receiver), name, arguments }
                if **receiver == Expression::Local(iterator.clone()) && name == method && arguments.is_empty()
        )
    };
    let Some(Statement::Declare {
        type_ref,
        name: element,
        value: Some(next),
    }) = body.first()
    else {
        return false;
    };
    let takes_next = match next {
        Expression::Cast { operand, .. } => call_on_iterator(operand, "next"),
        other => call_on_iterator(other, "next"),
    };
    let is_iterator = iterator_method == "iterator" && iterator_arguments.is_empty();
    let hidden = iterator.starts_with("var")
        && unmentioned(&body[1..], iterator)
        && unmentioned(&statements[index + 1..], iterator);
    if !(is_iterator && call_on_iterator(condition, "hasNext") && takes_next && hidden) {
        return false;
    }
    let looped = Statement::Loop {
        label: *label,
        kind: LoopKind::ForEach {
            type_ref: type_ref.clone(),
            name: element.clone(),
            iterable: (**iterable).clone(),
        },
        body: body[1..].to_vec(),
    };
    statements.splice(index - 1..=index, [looped]);
    true
}

/// `var s = E; var t = -1; switch (s.hashCode()) { case h: if
/// (s.equals("a")) { t = 0; } … } switch (t) { case 0: … }` is
/// `switch (E) { case "a": … }`.
fn form_string_switch(statements: &mut Vec<Statement>, index: usize) -> bool {
    if index < 2 || index + 1 >= statements.len() {
        return false;
    }
    let Statement::Declare {
        name: copy,
        value: Some(switched),
        ..
    } = &statements[index - 2]
    else {
        return false;
    };
    let Statement::Declare {
        name: chosen,
        value: Some(Expression::Literal(Literal::Int(-1))),
        ..
    } = &statements[index - 1]
    else {
        return false;
    };
    let Statement::Switch {
        value: hashed,
        cases: hash_cases,
    } = &statements[index]
    else {
        return false;
    };
    let hashes_the_copy = matches!(
        hashed,
        Expression::Call { receiver: Receiver::Instance(receiver), name, arguments }
            if **receiver == Expression::Local(copy.clone()) && name == "hashCode" && arguments.is_empty()
    );
    let Statement::Switch {
        value: Expression::Local(index_variable),
        cases,
    } = &statements[index + 1]
    else {
        return false;
    };
    if !hashes_the_copy || index_variable != chosen {
        return false;
    }
    let mut strings: Vec<(i32, String)> = Vec::new();
    for case in hash_cases {
        if !strings_chosen(&case.body, copy, chosen, &mut strings) {
            return false;
        }
    }
    let mut labeled = Vec::with_capacity(cases.len());
    for case in cases {
        let mut values = Vec::with_capacity(case.values.len());
        for value in &case.values {
            let Literal::Int(number) = value else {
                return false;
            };
            let Some((_, text)) = strings.iter().find(|(chosen, _)| chosen == number) else {
                return false;
            };
            values.push(Literal::String(text.clone()));
        }
        labeled.push(SwitchCase {
            values,
            default: case.default,
            body: case.body.clone(),
        });
    }
    let hidden = [copy, chosen].iter().all(|name| {
        name.starts_with("var")
            && unmentioned(&statements[index + 2..], name)
            && labeled.iter().all(|case| unmentioned(&case.body, name))
    });
    if !hidden {
        return false;
    }
    let switch = Statement::Switch {
        value: switched.clone(),
        cases: labeled,
    };
    statements.splice(index - 2..=index + 1, [switch]);
    true
}

/// The strings a hash case's `if (copy.equals("a")) { chosen = 0; }`
/// chain chooses, added to `strings`; `false` for anything else.
fn strings_chosen(
    statements: &[Statement],
    copy: &str,
    chosen: &str,
    strings: &mut Vec<(i32, String)>,
) -> bool {
    for statement in statements {
        match statement {
            Statement::Break(None) => {}
            Statement::If {
                condition,
                then,
                otherwise,
            } => {
                let Expression::Call {
                    receiver: Receiver::Instance(receiver),
                    name,
                    arguments,
                } = condition
                else {
                    return false;
                };
                let [Expression::Literal(Literal::String(text))] = arguments.as_slice() else {
                    return false;
                };
                let compares_the_copy =
                    **receiver == Expression::Local(copy.to_owned()) && name == "equals";
                let chooses = match then.as_slice() {
                    [
                        Statement::Assign {
                            target: Expression::Local(target),
                            value: Expression::Literal(Literal::Int(number)),
                        },
                        rest @ ..,
                    ] if target == chosen
                        && rest
                            .iter()
                            .all(|statement| *statement == Statement::Break(None)) =>
                    {
                        Some(*number)
                    }
                    _ => None,
                };
                let Some(number) = chooses.filter(|_| compares_the_copy) else {
                    return false;
                };
                strings.push((number, text.clone()));
                if !strings_chosen(otherwise, copy, chosen, strings) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}
