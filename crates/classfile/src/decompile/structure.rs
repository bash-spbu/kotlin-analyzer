//! A body's blocks nested into structured statements, then tidied.
//!
//! Nesting follows Ramsey's "Beyond Relooper" (2022) for graphs whose
//! loops nest (reducible ones): a block is written inside the block that
//! dominates it; one more than one block reaches — or one reached leaving
//! a loop — is written after a labeled block that the jumps to it
//! `break`; a loop's head is a `while (true)` that the jumps back
//! `continue`. Every block's code ends in an explicit jump, so this is
//! always right; tidying then removes the jumps and labels a person
//! wouldn't write, and makes `while (c)`, `do … while (c)` and `else`
//! out of the forms they leave. Last, a variable used in more than one
//! branch is declared where all of them see it.

use std::collections::HashSet;

use super::graph::{Graph, reverse_postorder};
use super::machine::Exit;
use super::{
    Body, Expression, Label, Literal, LoopKind, Owner, Receiver, Statement, SwitchCase,
    Unsupported, negated,
};

pub(super) fn structure(mut graph: Graph) -> Result<Body, Unsupported> {
    thread_jumps(&mut graph);
    let analysis = Analysis::of(&graph)?;
    let mut nester = Nester {
        graph: &mut graph,
        analysis: &analysis,
    };
    let mut statements = nester.tree(0);
    for _ in 0..3 {
        tidy(&mut statements);
    }
    declare(&mut statements)?;
    super::shapes::form_source_shapes(&mut statements);
    unlabel(&mut statements, &mut Vec::new());
    drop_unused_loop_labels(&mut statements);
    renumber(&mut statements);
    if statements.iter().any(holds_comparison_result) {
        return Err(Unsupported::StackShape);
    }
    Ok(Body { statements })
}

/// A block that only jumps on (javac's `goto` for a `break` or
/// `continue`) is where it jumps: the jumps to it go there instead.
fn thread_jumps(graph: &mut Graph) {
    let mut kept: Vec<usize> = Vec::new();
    for region in &graph.regions {
        kept.push(region.entry);
        kept.extend(region.catches.iter().map(|catch| catch.handler));
    }
    loop {
        let empty_jump = (1..graph.nodes.len())
            .filter(|node| !kept.contains(node))
            .find_map(|node| match graph.nodes[node].as_ref() {
                Some(found) if found.statements.is_empty() => match found.exit {
                    Exit::Goto(target) if target != node => Some((node, target)),
                    _ => None,
                },
                _ => None,
            });
        let Some((node, target)) = empty_jump else {
            return;
        };
        graph.nodes[node] = None;
        for other in graph.nodes.iter_mut().flatten() {
            redirect(&mut other.exit, node, target);
        }
    }
}

fn redirect(exit: &mut Exit, from: usize, to: usize) {
    let moved = |target: &mut usize| {
        if *target == from {
            *target = to;
        }
    };
    match exit {
        Exit::Goto(target) => moved(target),
        Exit::Branch {
            if_true, if_false, ..
        } => {
            moved(if_true);
            moved(if_false);
        }
        Exit::Switch { cases, default, .. } => {
            for (_, target) in cases.iter_mut() {
                moved(target);
            }
            moved(default);
        }
        Exit::Leave => {}
    }
}

/// What nesting needs to know of the graph.
struct Analysis {
    /// By node: its place in reverse postorder (`usize::MAX`: unreached).
    position: Vec<usize>,
    /// Loop heads: the nodes of each one's loop.
    loops: Vec<(usize, Vec<bool>)>,
    /// `try` entries: the nodes inside and the handlers.
    regions: Vec<RegionShape>,
    /// Whether a node is written after a labeled block (`break`s reach
    /// it) rather than where the one jump to it is.
    merge: Vec<bool>,
    /// By node: the nodes written after labeled blocks around it, in
    /// order.
    followers: Vec<Vec<usize>>,
}

/// A `try` as nesting sees it.
#[derive(Clone)]
struct RegionShape {
    entry: usize,
    body: Vec<bool>,
    /// Each handler with the classes it catches.
    catches: Vec<(Vec<crate::ClassName>, usize)>,
}

impl Analysis {
    fn of(graph: &Graph) -> Result<Analysis, Unsupported> {
        let count = graph.nodes.len();
        let regions: Vec<RegionShape> = graph
            .regions
            .iter()
            .map(|region| RegionShape {
                entry: region.entry,
                body: region.body.clone(),
                catches: region
                    .catches
                    .iter()
                    .map(|catch| (catch.types.clone(), catch.handler))
                    .collect(),
            })
            .collect();
        let is_handler = |node: usize| {
            regions
                .iter()
                .any(|region| region.catches.iter().any(|(_, handler)| *handler == node))
        };
        let successors: Vec<Vec<usize>> = (0..count)
            .map(|node| {
                let mut targets = graph.successors(node);
                for region in regions.iter().filter(|region| region.entry == node) {
                    targets.extend(region.catches.iter().map(|(_, handler)| *handler));
                }
                targets.dedup();
                let mut unique = Vec::new();
                for target in targets {
                    if !unique.contains(&target) {
                        unique.push(target);
                    }
                }
                unique
            })
            .collect();
        let order = reverse_postorder(&successors, 0);
        let mut position = vec![usize::MAX; count];
        for (index, node) in order.iter().enumerate() {
            position[*node] = index;
        }
        let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); count];
        for node in &order {
            for successor in &successors[*node] {
                predecessors[*successor].push(*node);
            }
        }
        let dominators = immediate_dominators(&order, &predecessors, &position);
        let dominates = |above: usize, mut below: usize| loop {
            if below == above {
                return true;
            }
            if below == 0 || dominators[below] == below {
                return false;
            }
            below = dominators[below];
        };
        // Jumps back: to a node that dominates the jump, or the graph's
        // loops don't nest.
        let mut back_edges: Vec<(usize, usize)> = Vec::new();
        for node in &order {
            for successor in &successors[*node] {
                if position[*successor] <= position[*node] {
                    if !dominates(*successor, *node) {
                        return Err(Unsupported::Branches);
                    }
                    back_edges.push((*node, *successor));
                }
            }
        }
        let mut loops: Vec<(usize, Vec<bool>)> = Vec::new();
        for (source, head) in &back_edges {
            let index = match loops.iter().position(|(existing, _)| existing == head) {
                Some(index) => index,
                None => {
                    let mut body = vec![false; count];
                    body[*head] = true;
                    loops.push((*head, body));
                    loops.len() - 1
                }
            };
            let body = &mut loops[index].1;
            let mut pending = vec![*source];
            while let Some(node) = pending.pop() {
                if body[node] {
                    continue;
                }
                body[node] = true;
                pending.extend(&predecessors[node]);
            }
        }
        // Outermost first.
        loops.sort_by_key(|(_, body)| {
            std::cmp::Reverse(body.iter().filter(|inside| **inside).count())
        });
        // A `try` is entered at its entry only.
        for region in &regions {
            for node in order.iter().copied() {
                let entered_elsewhere = region.body[node]
                    && node != region.entry
                    && predecessors[node]
                        .iter()
                        .any(|predecessor| !region.body[*predecessor]);
                if entered_elsewhere {
                    return Err(Unsupported::ExceptionHandlers);
                }
            }
        }
        // Loops and `try`s alike: what is left from inside one is written
        // after it. Outermost first.
        let mut containers: Vec<(usize, &Vec<bool>)> = loops
            .iter()
            .map(|(head, body)| (*head, body))
            .chain(regions.iter().map(|region| (region.entry, &region.body)))
            .collect();
        containers.sort_by_key(|(_, body)| {
            std::cmp::Reverse(body.iter().filter(|inside| **inside).count())
        });
        let mut merge = vec![false; count];
        let mut followers: Vec<Vec<usize>> = vec![Vec::new(); count];
        for node in order.iter().copied().skip(1) {
            // A handler is written in its `catch`.
            if is_handler(node) {
                continue;
            }
            let forward: Vec<usize> = predecessors[node]
                .iter()
                .copied()
                .filter(|predecessor| position[*predecessor] < position[node])
                .collect();
            let leaves_a_container = forward.iter().any(|predecessor| {
                containers
                    .iter()
                    .any(|(_, body)| body[*predecessor] && !body[node])
            });
            merge[node] = forward.len() >= 2 || leaves_a_container;
            if !merge[node] {
                continue;
            }
            // Written after the outermost container it isn't in but its
            // dominator is.
            let mut placement = dominators[node];
            if let Some((head, _)) = containers
                .iter()
                .find(|(head, body)| body[placement] && !body[node] && *head != placement)
            {
                placement = *head;
            }
            followers[placement].push(node);
        }
        for list in followers.iter_mut() {
            list.sort_by_key(|node| position[*node]);
        }
        Ok(Analysis {
            position,
            loops,
            regions,
            merge,
            followers,
        })
    }

    fn region_of(&self, entry: usize) -> Option<&RegionShape> {
        self.regions.iter().find(|region| region.entry == entry)
    }

    fn loop_of(&self, head: usize) -> Option<&Vec<bool>> {
        self.loops
            .iter()
            .find(|(existing, _)| *existing == head)
            .map(|(_, body)| body)
    }
}

/// Cooper, Harvey and Kennedy's iteration: each node's immediate
/// dominator (the entry its own).
fn immediate_dominators(
    order: &[usize],
    predecessors: &[Vec<usize>],
    position: &[usize],
) -> Vec<usize> {
    let mut dominators = vec![usize::MAX; predecessors.len()];
    let Some(entry) = order.first() else {
        return dominators;
    };
    dominators[*entry] = *entry;
    let intersect = |dominators: &[usize], mut first: usize, mut second: usize| {
        while first != second {
            while position[first] > position[second] {
                first = dominators[first];
            }
            while position[second] > position[first] {
                second = dominators[second];
            }
        }
        first
    };
    let mut changed = true;
    while changed {
        changed = false;
        for node in order.iter().skip(1) {
            let mut processed = predecessors[*node]
                .iter()
                .copied()
                .filter(|predecessor| dominators[*predecessor] != usize::MAX);
            let Some(first) = processed.next() else {
                continue;
            };
            let mut dominator = first;
            for predecessor in processed {
                dominator = intersect(&dominators, predecessor, dominator);
            }
            if dominators[*node] != dominator {
                dominators[*node] = dominator;
                changed = true;
            }
        }
    }
    dominators
}

fn loop_label(head: usize) -> Label {
    Label(2 * head as u32)
}

fn block_label(node: usize) -> Label {
    Label(2 * node as u32 + 1)
}

struct Nester<'graph> {
    graph: &'graph mut Graph,
    analysis: &'graph Analysis,
}

impl Nester<'_> {
    /// `node` and all written inside or after it.
    fn tree(&mut self, node: usize) -> Vec<Statement> {
        let followers = self.analysis.followers[node].clone();
        let mut containers = Vec::new();
        if self.analysis.loop_of(node).is_some() {
            containers.push(Container::Loop);
        }
        if self.analysis.region_of(node).is_some() {
            containers.push(Container::Try);
        }
        self.contained(node, &followers, &containers)
    }

    /// `node`'s code in its loop and `try` (outermost first), each
    /// followed by the followers outside it.
    fn contained(
        &mut self,
        node: usize,
        followers: &[usize],
        containers: &[Container],
    ) -> Vec<Statement> {
        let Some((container, rest)) = containers.split_first() else {
            return self.within(node, followers);
        };
        let body = match container {
            Container::Loop => self.analysis.loop_of(node).cloned(),
            Container::Try => self
                .analysis
                .region_of(node)
                .map(|region| region.body.clone()),
        }
        .unwrap_or_default();
        let (inside, outside): (Vec<usize>, Vec<usize>) = followers
            .iter()
            .copied()
            .partition(|follower| body.get(*follower).copied().unwrap_or(false));
        let inner = self.contained(node, &inside, rest);
        let mut code = vec![match container {
            Container::Loop => Statement::Loop {
                label: Some(loop_label(node)),
                kind: LoopKind::Forever,
                body: inner,
            },
            Container::Try => {
                let catches = self
                    .analysis
                    .region_of(node)
                    .map(|region| region.catches.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(types, handler)| catch_clause(types, self.tree(handler)))
                    .collect();
                Statement::Try {
                    body: inner,
                    catches,
                }
            }
        }];
        for follower in outside {
            code = vec![Statement::Block {
                label: block_label(follower),
                body: code,
            }];
            code.extend(self.tree(follower));
        }
        code
    }

    /// `node`'s code inside labeled blocks, each followed by a follower's.
    fn within(&mut self, node: usize, followers: &[usize]) -> Vec<Statement> {
        let Some((last, rest)) = followers.split_last() else {
            let Some(graph_node) = self.graph.nodes[node].as_mut() else {
                return Vec::new();
            };
            let mut code = std::mem::take(&mut graph_node.statements);
            let exit = std::mem::replace(&mut graph_node.exit, Exit::Leave);
            code.extend(self.exit_code(node, exit));
            return code;
        };
        let mut code = vec![Statement::Block {
            label: block_label(*last),
            body: self.within(node, rest),
        }];
        code.extend(self.tree(*last));
        code
    }

    /// A jump from `from` to `to`: `continue` back to a loop's head,
    /// `break` to a node written after a block, else `to` itself, here.
    fn branch(&mut self, from: usize, to: usize) -> Vec<Statement> {
        let position = &self.analysis.position;
        if position[to] <= position[from] {
            return vec![Statement::Continue(Some(loop_label(to)))];
        }
        if self.analysis.merge[to] {
            return vec![Statement::Break(Some(block_label(to)))];
        }
        self.tree(to)
    }

    fn exit_code(&mut self, node: usize, exit: Exit) -> Vec<Statement> {
        match exit {
            Exit::Leave => Vec::new(),
            Exit::Goto(target) => self.branch(node, target),
            Exit::Branch {
                if_true, if_false, ..
            } if if_true == if_false => self.branch(node, if_true),
            // The jump is taken when the condition holds: what falls
            // through is what the source wrote first.
            Exit::Branch {
                condition,
                if_true,
                if_false,
            } => vec![Statement::If {
                condition: negated(condition),
                then: self.branch(node, if_false),
                otherwise: self.branch(node, if_true),
            }],
            Exit::Switch {
                value,
                cases,
                default,
            } => {
                let mut grouped: Vec<(usize, Vec<i32>)> = Vec::new();
                for (key, target) in cases {
                    match grouped.iter_mut().find(|(existing, _)| *existing == target) {
                        Some((_, keys)) => keys.push(key),
                        None => grouped.push((target, vec![key])),
                    }
                }
                if !grouped.iter().any(|(target, _)| *target == default) {
                    grouped.push((default, Vec::new()));
                }
                let cases = grouped
                    .into_iter()
                    .map(|(target, values)| SwitchCase {
                        values: values.into_iter().map(Literal::Int).collect(),
                        default: target == default,
                        body: self.branch(node, target),
                    })
                    .collect();
                vec![Statement::Switch { value, cases }]
            }
        }
    }
}

/// What a node's code is inside of.
#[derive(Clone, Copy)]
enum Container {
    Loop,
    Try,
}

/// A handler's code as a `catch`: its first statement stores the caught
/// exception — the variable the `catch` declares; one that drops it
/// catches into `ignored`.
fn catch_clause(types: Vec<crate::ClassName>, mut body: Vec<Statement>) -> super::CatchClause {
    let caught = Expression::Local(super::graph::CAUGHT.to_owned());
    let stored = match body.first() {
        Some(Statement::Declare {
            name,
            value: Some(value),
            ..
        }) if *value == caught => Some(name.clone()),
        _ => None,
    };
    let name = match stored {
        Some(name) => {
            body.remove(0);
            name
        }
        None => {
            let renamed = Expression::Local("caught".to_owned());
            super::substitute_local(&mut body, super::graph::CAUGHT, &renamed);
            let used = body.iter().any(|statement| mentions(statement, "caught"));
            if used { "caught" } else { "ignored" }.to_owned()
        }
    };
    super::CatchClause { types, name, body }
}

/// Removes the jumps and labels a person wouldn't write, and makes loops
/// and `if`s the forms they were.
fn tidy(statements: &mut Vec<Statement>) {
    inline_followers(statements);
    strip_tail_jumps(statements, &[]);
    retarget_breaks(statements);
    unlabel(statements, &mut Vec::new());
    splice_unreferenced(statements);
    form_loops(statements);
    strip_tail_jumps(statements, &[]);
    unlabel(statements, &mut Vec::new());
    form_ifs(statements);
    splice_unreferenced(statements);
}

/// `label: { … break label; … } rest`, where `rest` never goes on past its
/// end and nothing else breaks to it: `rest` where the `break` was.
fn inline_followers(statements: &mut Vec<Statement>) {
    for statement in statements.iter_mut() {
        for child in children(statement) {
            inline_followers(child);
        }
    }
    let mut index = 0;
    while index < statements.len() {
        let rest = &statements[index + 1..];
        let movable = !rest.is_empty() && ends_in_jump(rest);
        let single_break = match &statements[index] {
            Statement::Block { label, body } => {
                // A loop's own exit stays after the loop: `while (c)`.
                let exits_its_loop = match body.last() {
                    Some(Statement::Loop {
                        body: loop_body, ..
                    }) => !breaks_from_nested_loop(loop_body, *label),
                    _ => false,
                };
                movable && !exits_its_loop && break_count(body, *label) == 1
            }
            _ => false,
        };
        if single_break {
            let rest: Vec<Statement> = statements.drain(index + 1..).collect();
            if let Statement::Block { label, body } = &mut statements[index] {
                let label = *label;
                put_in_place_of_break(body, label, rest);
            }
        }
        index += 1;
    }
}

/// Whether a `break label` in `statements` is inside a loop nested in them.
fn breaks_from_nested_loop(statements: &[Statement], label: Label) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Loop { body, .. } => break_count(body, label) > 0,
        Statement::If {
            then, otherwise, ..
        } => breaks_from_nested_loop(then, label) || breaks_from_nested_loop(otherwise, label),
        Statement::Try { body, catches } => {
            breaks_from_nested_loop(body, label)
                || catches
                    .iter()
                    .any(|clause| breaks_from_nested_loop(&clause.body, label))
        }
        Statement::Block { body, .. } => breaks_from_nested_loop(body, label),
        Statement::Switch { cases, .. } => cases
            .iter()
            .any(|case| breaks_from_nested_loop(&case.body, label)),
        _ => false,
    })
}

/// How many `break label`s `statements` hold, however nested.
fn break_count(statements: &[Statement], label: Label) -> usize {
    statements
        .iter()
        .map(|statement| match statement {
            Statement::Break(Some(target)) => usize::from(*target == label),
            Statement::Continue(Some(target)) => usize::from(*target == label) * 2,
            Statement::If {
                then, otherwise, ..
            } => break_count(then, label) + break_count(otherwise, label),
            Statement::Try { body, catches } => {
                break_count(body, label)
                    + catches
                        .iter()
                        .map(|clause| break_count(&clause.body, label))
                        .sum::<usize>()
            }
            Statement::Loop { body, .. } | Statement::Block { body, .. } => {
                break_count(body, label)
            }
            Statement::Switch { cases, .. } => cases
                .iter()
                .map(|case| break_count(&case.body, label))
                .sum(),
            _ => 0,
        })
        .sum()
}

/// The `break label` in `statements` replaced by `code`.
fn put_in_place_of_break(
    statements: &mut Vec<Statement>,
    label: Label,
    code: Vec<Statement>,
) -> Option<Vec<Statement>> {
    let mut code = Some(code);
    let mut index = 0;
    while index < statements.len() {
        if statements[index] == Statement::Break(Some(label)) {
            statements.remove(index);
            let inserted = code.take().unwrap_or_default();
            let count = inserted.len();
            for (offset, statement) in inserted.into_iter().enumerate() {
                statements.insert(index + offset, statement);
            }
            index += count;
            continue;
        }
        for child in children(&mut statements[index]) {
            if let Some(remaining) = code.take() {
                code = put_in_place_of_break(child, label, remaining);
            }
        }
        index += 1;
    }
    code
}

/// Each statement's lists of statements.
pub(super) fn children(statement: &mut Statement) -> Vec<&mut Vec<Statement>> {
    match statement {
        Statement::If {
            then, otherwise, ..
        } => vec![then, otherwise],
        Statement::Try { body, catches } => {
            let mut lists = vec![body];
            lists.extend(catches.iter_mut().map(|clause| &mut clause.body));
            lists
        }
        Statement::Loop { body, .. } | Statement::Block { body, .. } => vec![body],
        Statement::Switch { cases, .. } => cases.iter_mut().map(|case| &mut case.body).collect(),
        _ => Vec::new(),
    }
}

/// Drops a jump that goes where falling off the end of its list goes
/// anyway: `redundant` says which do, for `statements`' end.
fn strip_tail_jumps(statements: &mut Vec<Statement>, redundant: &[Statement]) {
    while statements
        .last()
        .is_some_and(|last| redundant.contains(last))
    {
        statements.pop();
    }
    let last = statements.len().wrapping_sub(1);
    for (index, statement) in statements.iter_mut().enumerate() {
        let at_tail = index == last;
        let inherited: &[Statement] = if at_tail { redundant } else { &[] };
        match statement {
            Statement::If {
                then, otherwise, ..
            } => {
                strip_tail_jumps(then, inherited);
                strip_tail_jumps(otherwise, inherited);
            }
            Statement::Try { body, catches } => {
                strip_tail_jumps(body, inherited);
                for clause in catches {
                    strip_tail_jumps(&mut clause.body, inherited);
                }
            }
            Statement::Block { label, body } => {
                let mut own = vec![Statement::Break(Some(*label))];
                own.extend_from_slice(inherited);
                strip_tail_jumps(body, &own);
            }
            Statement::Loop { label, body, .. } => {
                let mut own = vec![Statement::Continue(None)];
                if let Some(label) = label {
                    own.push(Statement::Continue(Some(*label)));
                }
                strip_tail_jumps(body, &own);
            }
            Statement::Switch { cases, .. } => {
                let case_count = cases.len();
                for (position, case) in cases.iter_mut().enumerate() {
                    strip_tail_jumps(&mut case.body, &[]);
                    // Out of the switch is where the jump goes: `break`.
                    if at_tail
                        && case
                            .body
                            .last()
                            .is_some_and(|last| redundant.contains(last))
                    {
                        case.body.pop();
                        case.body.push(Statement::Break(None));
                    }
                    let is_last_case = position + 1 == case_count;
                    if is_last_case && case.body.last() == Some(&Statement::Break(None)) {
                        case.body.pop();
                    }
                }
            }
            _ => {}
        }
    }
}

/// `label: { …; loop }`: a `break label` inside the loop leaves the loop —
/// `break` of the loop. The same for a switch.
fn retarget_breaks(statements: &mut [Statement]) {
    for statement in statements.iter_mut() {
        if let Statement::Block { label, body } = statement {
            let label = *label;
            match body.last_mut() {
                Some(Statement::Loop {
                    label: Some(loop_label),
                    body: loop_body,
                    ..
                }) => {
                    let loop_label = *loop_label;
                    replace_breaks(loop_body, label, Statement::Break(Some(loop_label)));
                }
                Some(Statement::Switch { cases, .. }) => {
                    for case in cases {
                        replace_breaks(&mut case.body, label, Statement::Break(None));
                    }
                }
                _ => {}
            }
        }
        for child in children(statement) {
            retarget_breaks(child);
        }
    }
}

/// `break label` in `statements` as `replacement` — inside a nested loop
/// or switch only when the replacement names its target, where an
/// unlabeled `break` would mean something else.
fn replace_breaks(statements: &mut [Statement], label: Label, replacement: Statement) {
    let names_its_target = matches!(replacement, Statement::Break(Some(_)));
    for statement in statements.iter_mut() {
        match statement {
            Statement::Break(Some(target)) if *target == label => *statement = replacement.clone(),
            Statement::If {
                then, otherwise, ..
            } => {
                replace_breaks(then, label, replacement.clone());
                replace_breaks(otherwise, label, replacement.clone());
            }
            Statement::Try { body, catches } => {
                replace_breaks(body, label, replacement.clone());
                for clause in catches {
                    replace_breaks(&mut clause.body, label, replacement.clone());
                }
            }
            Statement::Block { body, .. } => replace_breaks(body, label, replacement.clone()),
            Statement::Loop { body, .. } if names_its_target => {
                replace_breaks(body, label, replacement.clone())
            }
            Statement::Switch { cases, .. } if names_its_target => {
                for case in cases {
                    replace_breaks(&mut case.body, label, replacement.clone());
                }
            }
            _ => {}
        }
    }
}

/// What a `break`/`continue` without a label means where it is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Breakable {
    Loop(Option<Label>),
    Switch,
}

/// `break label`/`continue label` of the innermost loop: without it.
pub(super) fn unlabel(statements: &mut [Statement], enclosing: &mut Vec<Breakable>) {
    for statement in statements.iter_mut() {
        match statement {
            Statement::Break(Some(label)) => {
                if enclosing.last() == Some(&Breakable::Loop(Some(*label))) {
                    *statement = Statement::Break(None);
                }
            }
            Statement::Continue(Some(label)) => {
                let innermost_loop = enclosing
                    .iter()
                    .rev()
                    .find(|breakable| matches!(breakable, Breakable::Loop(_)));
                if innermost_loop == Some(&Breakable::Loop(Some(*label))) {
                    *statement = Statement::Continue(None);
                }
            }
            Statement::Loop { label, body, .. } => {
                enclosing.push(Breakable::Loop(*label));
                unlabel(body, enclosing);
                enclosing.pop();
            }
            Statement::Switch { cases, .. } => {
                enclosing.push(Breakable::Switch);
                for case in cases {
                    unlabel(&mut case.body, enclosing);
                }
                enclosing.pop();
            }
            Statement::If {
                then, otherwise, ..
            } => {
                unlabel(then, enclosing);
                unlabel(otherwise, enclosing);
            }
            Statement::Try { body, catches } => {
                unlabel(body, enclosing);
                for clause in catches {
                    unlabel(&mut clause.body, enclosing);
                }
            }
            Statement::Block { body, .. } => unlabel(body, enclosing),
            _ => {}
        }
    }
}

/// Whether anything in `statements` jumps to `label`.
fn references(statements: &[Statement], label: Label) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Break(Some(target)) | Statement::Continue(Some(target)) => *target == label,
        Statement::If {
            then, otherwise, ..
        } => references(then, label) || references(otherwise, label),
        Statement::Try { body, catches } => try_statements(body, catches)
            .any(|inner| references(std::slice::from_ref(inner), label)),
        Statement::Loop { body, .. } | Statement::Block { body, .. } => references(body, label),
        Statement::Switch { cases, .. } => cases.iter().any(|case| references(&case.body, label)),
        _ => false,
    })
}

/// A loop nothing names has no label (last: until then, a `break` may
/// be retargeted to it).
fn drop_unused_loop_labels(statements: &mut [Statement]) {
    for statement in statements.iter_mut() {
        if let Statement::Loop { label, body, .. } = statement
            && label.is_some_and(|label| !references(body, label))
        {
            *label = None;
        }
        for child in children(statement) {
            drop_unused_loop_labels(child);
        }
    }
}

/// A block nothing breaks out of is its statements.
fn splice_unreferenced(statements: &mut Vec<Statement>) {
    let mut index = 0;
    while index < statements.len() {
        for child in children(&mut statements[index]) {
            splice_unreferenced(child);
        }
        let unreferenced_block = matches!(
            &statements[index],
            Statement::Block { label, body } if !references(body, *label)
        );
        if unreferenced_block {
            let Statement::Block { body, .. } = statements.remove(index) else {
                unreachable!("matched above");
            };
            let spliced = body.len();
            for (offset, inner) in body.into_iter().enumerate() {
                statements.insert(index + offset, inner);
            }
            index += spliced;
            continue;
        }
        index += 1;
    }
}

/// Whether `statements`, outside nested loops, continue the loop they
/// are the body of.
pub(super) fn continues(statements: &[Statement], label: Option<Label>) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Continue(None) => true,
        Statement::Continue(Some(target)) => Some(*target) == label,
        Statement::If {
            then, otherwise, ..
        } => continues(then, label) || continues(otherwise, label),
        Statement::Try { body, catches } => {
            continues(body, label) || catches.iter().any(|clause| continues(&clause.body, label))
        }
        Statement::Block { body, .. } => continues(body, label),
        Statement::Switch { cases, .. } => cases.iter().any(|case| continues(&case.body, label)),
        // Inside a nested loop only `continue label` continues this one.
        Statement::Loop { body, .. } => label.is_some_and(|label| continues_labeled(body, label)),
        _ => false,
    })
}

/// Whether `statements` hold `continue label`, however nested.
fn continues_labeled(statements: &[Statement], label: Label) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Continue(Some(target)) => *target == label,
        Statement::If {
            then, otherwise, ..
        } => continues_labeled(then, label) || continues_labeled(otherwise, label),
        Statement::Try { body, catches } => try_statements(body, catches)
            .any(|inner| continues_labeled(std::slice::from_ref(inner), label)),
        Statement::Loop { body, .. } | Statement::Block { body, .. } => {
            continues_labeled(body, label)
        }
        Statement::Switch { cases, .. } => cases
            .iter()
            .any(|case| continues_labeled(&case.body, label)),
        _ => false,
    })
}

/// `while (true) { if (!c) break; … }` is `while (c) { … }`;
/// `while (true) { …; if (!c) break; }` is `do { … } while (c);`.
fn form_loops(statements: &mut [Statement]) {
    for statement in statements.iter_mut() {
        for child in children(statement) {
            form_loops(child);
        }
        let Statement::Loop {
            label,
            kind: kind @ LoopKind::Forever,
            body,
        } = statement
        else {
            continue;
        };
        let leaves = |list: &[Statement]| list == [Statement::Break(None)];
        if let Some(Statement::If {
            condition,
            then,
            otherwise,
        }) = body.first()
        {
            let stays = if leaves(otherwise) {
                Some((condition.clone(), then.clone()))
            } else if leaves(then) {
                Some((negated(condition.clone()), otherwise.clone()))
            } else {
                None
            };
            if let Some((condition, rest)) = stays {
                let mut new_body = rest;
                new_body.extend(body.drain(1..));
                *body = new_body;
                *kind = LoopKind::While(condition);
                continue;
            }
        }
        let continued = continues(body, *label);
        if let Some(Statement::If {
            condition,
            then,
            otherwise,
        }) = body.last()
            && !continued
        {
            let repeat_when = if leaves(then) && otherwise.is_empty() {
                Some(negated(condition.clone()))
            } else if leaves(otherwise) && then.is_empty() {
                Some(condition.clone())
            } else {
                None
            };
            if let Some(repeat_when) = repeat_when {
                body.pop();
                *kind = LoopKind::DoWhile(repeat_when);
            }
        }
    }
}

/// Whether control never goes on past `statements`' end.
fn ends_in_jump(statements: &[Statement]) -> bool {
    match statements.last() {
        Some(
            Statement::Return(_)
            | Statement::Throw(_)
            | Statement::Break(_)
            | Statement::Continue(_),
        ) => true,
        Some(Statement::If {
            then, otherwise, ..
        }) => ends_in_jump(then) && ends_in_jump(otherwise),
        Some(Statement::Try { body, catches }) => {
            ends_in_jump(body) && catches.iter().all(|clause| ends_in_jump(&clause.body))
        }
        _ => false,
    }
}

/// `if (c) {} else { … }` is `if (!c) { … }`; an `else` after a `then`
/// that never goes on is what comes after the `if`.
fn form_ifs(statements: &mut Vec<Statement>) {
    let mut index = 0;
    while index < statements.len() {
        for child in children(&mut statements[index]) {
            form_ifs(child);
        }
        if let Statement::If {
            condition,
            then,
            otherwise,
        } = &mut statements[index]
        {
            if then.is_empty() && !otherwise.is_empty() {
                *condition = negated(condition.clone());
                std::mem::swap(then, otherwise);
            }
            let flattens = ends_in_jump(then) && !otherwise.is_empty() && !is_else_if(otherwise);
            if flattens {
                let after = std::mem::take(otherwise);
                let count = after.len();
                for (offset, statement) in after.into_iter().enumerate() {
                    statements.insert(index + 1 + offset, statement);
                }
                index += 1;
                let _ = count;
                continue;
            }
        }
        index += 1;
    }
}

fn is_else_if(statements: &[Statement]) -> bool {
    matches!(statements, [Statement::If { .. }])
}

/// Declares each variable where all its uses see it: where it is first
/// stored, if the rest come after in the same list; else, before the
/// first statement using it, its stores then assignments.
fn declare(statements: &mut Vec<Statement>) -> Result<(), Unsupported> {
    let mut names: Vec<String> = Vec::new();
    for statement in statements.iter() {
        declared_names(statement, &mut names);
    }
    let mut hoisted: HashSet<String> = HashSet::new();
    for name in &names {
        let positions: Vec<usize> = (0..statements.len())
            .filter(|index| mentions(&statements[*index], name))
            .collect();
        let Some(first) = positions.first().copied() else {
            continue;
        };
        let declared_first = matches!(
            &statements[first],
            Statement::Declare { name: declared, .. } if declared == name
        );
        let declarations: usize = statements
            .iter()
            .map(|statement| declarations_of(statement, name))
            .sum();
        let within_one = positions.len() == 1 && !declared_first;
        let already_right = declared_first && declarations == 1;
        if within_one || already_right {
            continue;
        }
        let type_ref = statements
            .iter()
            .find_map(|statement| declared_type(statement, name))
            .ok_or(Unsupported::UntypedVariable)?;
        undeclare(statements, name);
        statements.insert(
            first,
            Statement::Declare {
                type_ref: Some(type_ref),
                name: name.clone(),
                value: None,
            },
        );
        hoisted.insert(name.clone());
    }
    for statement in statements.iter_mut() {
        for child in children(statement) {
            declare(child)?;
        }
    }
    Ok(())
}

fn declared_names(statement: &Statement, names: &mut Vec<String>) {
    match statement {
        Statement::Declare { name, .. } => {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        Statement::If {
            then, otherwise, ..
        } => {
            then.iter().for_each(|inner| declared_names(inner, names));
            otherwise
                .iter()
                .for_each(|inner| declared_names(inner, names));
        }
        Statement::Try { body, catches } => {
            try_statements(body, catches).for_each(|inner| declared_names(inner, names))
        }
        Statement::Loop { body, .. } | Statement::Block { body, .. } => {
            body.iter().for_each(|inner| declared_names(inner, names))
        }
        Statement::Switch { cases, .. } => cases.iter().for_each(|case| {
            case.body
                .iter()
                .for_each(|inner| declared_names(inner, names))
        }),
        _ => {}
    }
}

fn declarations_of(statement: &Statement, name: &str) -> usize {
    match statement {
        Statement::Declare { name: declared, .. } => usize::from(declared == name),
        Statement::If {
            then, otherwise, ..
        } => then
            .iter()
            .chain(otherwise)
            .map(|inner| declarations_of(inner, name))
            .sum(),
        Statement::Try { body, catches } => try_statements(body, catches)
            .map(|inner| declarations_of(inner, name))
            .sum(),
        Statement::Loop { body, .. } | Statement::Block { body, .. } => {
            body.iter().map(|inner| declarations_of(inner, name)).sum()
        }
        Statement::Switch { cases, .. } => cases
            .iter()
            .flat_map(|case| &case.body)
            .map(|inner| declarations_of(inner, name))
            .sum(),
        _ => 0,
    }
}

fn declared_type(statement: &Statement, name: &str) -> Option<crate::LibraryType> {
    match statement {
        Statement::Declare {
            name: declared,
            type_ref,
            ..
        } if declared == name => type_ref.clone(),
        Statement::If {
            then, otherwise, ..
        } => then
            .iter()
            .chain(otherwise)
            .find_map(|inner| declared_type(inner, name)),
        Statement::Try { body, catches } => {
            try_statements(body, catches).find_map(|inner| declared_type(inner, name))
        }
        Statement::Loop { body, .. } | Statement::Block { body, .. } => {
            body.iter().find_map(|inner| declared_type(inner, name))
        }
        Statement::Switch { cases, .. } => cases
            .iter()
            .flat_map(|case| &case.body)
            .find_map(|inner| declared_type(inner, name)),
        _ => None,
    }
}

/// Each declaration of `name` in `statements` as an assignment.
fn undeclare(statements: &mut Vec<Statement>, name: &str) {
    statements.retain(|statement| {
        !matches!(statement, Statement::Declare { name: declared, value: None, .. } if declared == name)
    });
    for statement in statements.iter_mut() {
        if let Statement::Declare {
            name: declared,
            value: Some(value),
            ..
        } = statement
            && declared == name
        {
            *statement = Statement::Assign {
                target: Expression::Local(name.to_owned()),
                value: value.clone(),
            };
        }
        for child in children(statement) {
            undeclare(child, name);
        }
    }
}

/// Whether `statement` reads, writes or declares the variable `name`.
pub(super) fn mentions(statement: &Statement, name: &str) -> bool {
    let in_expression = |expression: &Expression| expression_mentions(expression, name);
    match statement {
        Statement::Expression(expression)
        | Statement::Return(Some(expression))
        | Statement::Throw(expression) => in_expression(expression),
        Statement::Declare {
            name: declared,
            value,
            ..
        } => declared == name || value.as_ref().is_some_and(in_expression),
        Statement::Assign { target, value } => in_expression(target) || in_expression(value),
        Statement::Increment { name: variable, .. } => variable == name,
        Statement::SuperConstructor(arguments) | Statement::ThisConstructor(arguments) => {
            arguments.iter().any(in_expression)
        }
        Statement::If {
            condition,
            then,
            otherwise,
        } => {
            in_expression(condition)
                || then
                    .iter()
                    .chain(otherwise)
                    .any(|inner| mentions(inner, name))
        }
        Statement::Loop { kind, body, .. } => {
            let in_header = match kind {
                LoopKind::Forever => false,
                LoopKind::While(condition) | LoopKind::DoWhile(condition) => {
                    in_expression(condition)
                }
                LoopKind::For {
                    initializer,
                    condition,
                    update,
                } => {
                    initializer
                        .as_ref()
                        .is_some_and(|initializer| mentions(initializer, name))
                        || in_expression(condition)
                        || mentions(update, name)
                }
                LoopKind::ForEach {
                    name: variable,
                    iterable,
                    ..
                } => variable == name || in_expression(iterable),
            };
            in_header || body.iter().any(|inner| mentions(inner, name))
        }
        Statement::Try { body, catches } => {
            try_statements(body, catches).any(|inner| mentions(inner, name))
        }
        Statement::Block { body, .. } => body.iter().any(|inner| mentions(inner, name)),
        Statement::Switch { value, cases } => {
            in_expression(value)
                || cases
                    .iter()
                    .flat_map(|case| &case.body)
                    .any(|inner| mentions(inner, name))
        }
        Statement::Return(None) | Statement::Break(_) | Statement::Continue(_) => false,
    }
}

/// Every expression directly inside an expression.
fn operands(expression: &Expression) -> Vec<&Expression> {
    match expression {
        Expression::This
        | Expression::Local(_)
        | Expression::Literal(_)
        | Expression::ClassLiteral(_)
        | Expression::PostIncrement { .. } => Vec::new(),
        Expression::ArrayInitializer { values, .. } => values.iter().collect(),
        Expression::Field { owner, .. } => match owner {
            Owner::Instance(object) => vec![object.as_ref()],
            Owner::Static(_) => Vec::new(),
        },
        Expression::Call {
            receiver,
            arguments,
            ..
        } => {
            let mut all: Vec<&Expression> = arguments.iter().collect();
            if let Receiver::Instance(object) = receiver {
                all.push(object);
            }
            all
        }
        Expression::New { arguments, .. } => arguments.iter().collect(),
        Expression::Concatenation(pieces) => pieces.iter().collect(),
        Expression::NewArray { length, .. } => vec![length.as_ref()],
        Expression::ArrayElement { array, index } => vec![array.as_ref(), index.as_ref()],
        Expression::ArrayLength(operand)
        | Expression::Negate(operand)
        | Expression::Not(operand)
        | Expression::Cast { operand, .. }
        | Expression::InstanceOf { operand, .. }
        | Expression::Boxed { value: operand, .. }
        | Expression::Unboxed { value: operand, .. } => vec![operand.as_ref()],
        Expression::Binary { left, right, .. }
        | Expression::Logical { left, right, .. }
        | Expression::Compare { left, right, .. }
        | Expression::CompareResult { left, right, .. } => vec![left.as_ref(), right.as_ref()],
        Expression::Lambda { body, .. } => match body {
            super::LambdaBody::Expression(value) => vec![value.as_ref()],
            // Its statements: `expression_mentions` reads them.
            super::LambdaBody::Block(_) => Vec::new(),
        },
        Expression::MethodReference { target, .. } => match target {
            super::ReferenceTarget::Bound(receiver) => vec![receiver.as_ref()],
            _ => Vec::new(),
        },
        Expression::Conditional {
            condition,
            then,
            otherwise,
        } => vec![condition.as_ref(), then.as_ref(), otherwise.as_ref()],
    }
}

pub(super) fn expression_mentions(expression: &Expression, name: &str) -> bool {
    if matches!(expression, Expression::PostIncrement { name: variable, .. } if variable == name) {
        return true;
    }
    let in_block_lambda = matches!(
        expression,
        Expression::Lambda { body: super::LambdaBody::Block(statements), .. }
            if statements.iter().any(|statement| mentions(statement, name))
    );
    in_block_lambda
        || matches!(expression, Expression::Local(local) if local == name)
        || operands(expression)
            .into_iter()
            .any(|operand| expression_mentions(operand, name))
}

fn expression_holds_comparison_result(expression: &Expression) -> bool {
    matches!(expression, Expression::CompareResult { .. })
        || operands(expression)
            .into_iter()
            .any(expression_holds_comparison_result)
}

/// Whether a comparison instruction's result is left as a value (used
/// other than by a branch): not read.
fn holds_comparison_result(statement: &Statement) -> bool {
    let in_expression = expression_holds_comparison_result;
    match statement {
        Statement::Expression(expression)
        | Statement::Return(Some(expression))
        | Statement::Throw(expression) => in_expression(expression),
        Statement::Declare { value, .. } => value.as_ref().is_some_and(in_expression),
        Statement::Assign { target, value } => in_expression(target) || in_expression(value),
        Statement::SuperConstructor(arguments) | Statement::ThisConstructor(arguments) => {
            arguments.iter().any(in_expression)
        }
        Statement::If {
            condition,
            then,
            otherwise,
        } => in_expression(condition) || then.iter().chain(otherwise).any(holds_comparison_result),
        Statement::Loop { kind, body, .. } => {
            let in_header = match kind {
                LoopKind::Forever => false,
                LoopKind::While(condition) | LoopKind::DoWhile(condition) => {
                    in_expression(condition)
                }
                LoopKind::For {
                    initializer,
                    condition,
                    update,
                } => {
                    initializer
                        .as_ref()
                        .is_some_and(|initializer| holds_comparison_result(initializer))
                        || in_expression(condition)
                        || holds_comparison_result(update)
                }
                LoopKind::ForEach { iterable, .. } => in_expression(iterable),
            };
            in_header || body.iter().any(holds_comparison_result)
        }
        Statement::Try { body, catches } => {
            try_statements(body, catches).any(holds_comparison_result)
        }
        Statement::Block { body, .. } => body.iter().any(holds_comparison_result),
        Statement::Switch { value, cases } => {
            in_expression(value)
                || cases
                    .iter()
                    .flat_map(|case| &case.body)
                    .any(holds_comparison_result)
        }
        Statement::Increment { .. }
        | Statement::Return(None)
        | Statement::Break(_)
        | Statement::Continue(_) => false,
    }
}

/// Labels numbered 1, 2… in the order they appear.
fn renumber(statements: &mut [Statement]) {
    let mut numbers: Vec<Label> = Vec::new();
    renumber_in(statements, &mut numbers);
}

fn renumber_in(statements: &mut [Statement], numbers: &mut Vec<Label>) {
    let number = |label: &mut Label, numbers: &mut Vec<Label>| {
        let index = match numbers.iter().position(|known| known == label) {
            Some(index) => index,
            None => {
                numbers.push(*label);
                numbers.len() - 1
            }
        };
        *label = Label(index as u32 + 1);
    };
    for statement in statements.iter_mut() {
        match statement {
            Statement::Loop {
                label: Some(label), ..
            }
            | Statement::Block { label, .. }
            | Statement::Break(Some(label))
            | Statement::Continue(Some(label)) => number(label, numbers),
            _ => {}
        }
        for child in children(statement) {
            renumber_in(child, numbers);
        }
    }
}

/// A `try`'s statements and its `catch`es'.
fn try_statements<'statements>(
    body: &'statements [Statement],
    catches: &'statements [super::CatchClause],
) -> impl Iterator<Item = &'statements Statement> {
    body.iter()
        .chain(catches.iter().flat_map(|clause| clause.body.iter()))
}
