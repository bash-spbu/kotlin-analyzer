//! A body's basic blocks, replayed in order, with what joins of the stack
//! and chains of conditions were written as: values two branches leave
//! for one block are `?:`; a condition whose block only tests another,
//! jumping to one of the same places, is `&&` or `||`.

use super::Unsupported;
use super::machine::{Exit, Machine, Replayed, Value};
use super::{Expression, LogicalOperator, negated};

/// The body's blocks after replaying and joining: what `structure` nests.
pub(super) struct Graph {
    /// By block; `None` for one joined into another or never reached.
    /// Their exits name blocks by index.
    pub(super) nodes: Vec<Option<Node>>,
}

pub(super) struct Node {
    pub(super) statements: Vec<super::Statement>,
    pub(super) exit: Exit,
}

impl Graph {
    pub(super) fn successors(&self, node: usize) -> Vec<usize> {
        match self.nodes[node].as_ref().map(|node| &node.exit) {
            Some(exit) => targets_of(exit),
            None => Vec::new(),
        }
    }
}

pub(super) fn targets_of(exit: &Exit) -> Vec<usize> {
    match exit {
        Exit::Goto(target) => vec![*target],
        Exit::Branch {
            if_true, if_false, ..
        } => vec![*if_false, *if_true],
        Exit::Switch { cases, default, .. } => {
            let mut targets: Vec<usize> = cases.iter().map(|(_, target)| *target).collect();
            targets.push(*default);
            targets
        }
        Exit::Leave => Vec::new(),
    }
}

/// An instruction as the graph needs it: where it is, how long, where it
/// may go.
struct Decoded {
    pc: usize,
    length: usize,
    /// Jump targets.
    targets: Vec<usize>,
    /// Whether the next instruction is reached only by a jump.
    ends_flow: bool,
    /// Whether it ends its block (a jump, `switch`, return, throw).
    ends_block: bool,
}

pub(super) fn build(mut machine: Machine<'_>) -> Result<Graph, Unsupported> {
    let bytes = machine.code.bytes.clone();
    let decoded = decode(&bytes)?;
    let starts: Vec<usize> = decoded.iter().map(|instruction| instruction.pc).collect();
    let mut leaders: Vec<usize> = vec![0];
    for instruction in &decoded {
        leaders.extend(&instruction.targets);
        if instruction.ends_block {
            leaders.push(instruction.pc + instruction.length);
        }
    }
    leaders.retain(|leader| *leader < bytes.len());
    leaders.sort_unstable();
    leaders.dedup();
    if leaders
        .iter()
        .any(|leader| starts.binary_search(leader).is_err())
    {
        return Err(Unsupported::Malformed);
    }
    let block_of = |pc: usize| {
        leaders
            .binary_search(&pc)
            .map_err(|_| Unsupported::Malformed)
    };
    let ranges: Vec<(usize, usize)> = leaders
        .iter()
        .enumerate()
        .map(|(index, start)| {
            (
                *start,
                leaders.get(index + 1).copied().unwrap_or(bytes.len()),
            )
        })
        .collect();
    // Each block's successors, from its last instruction.
    let mut raw_successors: Vec<Vec<usize>> = Vec::with_capacity(ranges.len());
    for (start, end) in &ranges {
        let last = decoded
            .iter()
            .rev()
            .find(|instruction| instruction.pc >= *start && instruction.pc < *end)
            .ok_or(Unsupported::Malformed)?;
        let mut successors = Vec::new();
        for target in &last.targets {
            successors.push(block_of(*target)?);
        }
        if !last.ends_flow && *end < bytes.len() {
            successors.push(block_of(*end)?);
        }
        raw_successors.push(successors);
    }
    let order = reverse_postorder(&raw_successors, 0);
    let mut position = vec![usize::MAX; ranges.len()];
    for (index, block) in order.iter().enumerate() {
        position[*block] = index;
    }
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); ranges.len()];
    for block in &order {
        for successor in &raw_successors[*block] {
            if !predecessors[*successor].contains(block) {
                predecessors[*successor].push(*block);
            }
        }
    }
    let mut joining = Joining {
        replayed: vec![None; ranges.len()],
        alive: vec![false; ranges.len()],
        predecessors,
    };
    for block in &order {
        joining.alive[*block] = true;
    }
    for block in order.iter().copied() {
        let has_back_edge = joining.predecessors[block]
            .iter()
            .any(|predecessor| position[*predecessor] >= position[block]);
        let entry = joining.entry(block)?;
        if has_back_edge && !entry.is_empty() {
            return Err(Unsupported::StackShape);
        }
        let (start, end) = ranges[block];
        let mut replayed = machine.replay(start, end, entry)?;
        replayed.exit = match replayed.exit {
            Exit::Goto(pc) => Exit::Goto(block_of(pc)?),
            Exit::Branch {
                condition,
                if_true,
                if_false,
            } => Exit::Branch {
                condition,
                if_true: block_of(if_true)?,
                if_false: block_of(if_false)?,
            },
            Exit::Switch {
                value,
                cases,
                default,
            } => Exit::Switch {
                value,
                cases: cases
                    .into_iter()
                    .map(|(key, pc)| Ok((key, block_of(pc)?)))
                    .collect::<Result<_, Unsupported>>()?,
                default: block_of(default)?,
            },
            Exit::Leave => Exit::Leave,
        };
        joining.replayed[block] = Some(replayed);
        joining.join_condition(block);
    }
    // A value carried along a jump back is a loop's: not read.
    for block in &order {
        let Some(replayed) = joining.replayed[*block]
            .as_ref()
            .filter(|_| joining.alive[*block])
        else {
            continue;
        };
        let jumps_back = targets_of(&replayed.exit)
            .iter()
            .any(|target| position[*target] <= position[*block]);
        if jumps_back && !replayed.stack.is_empty() {
            return Err(Unsupported::StackShape);
        }
    }
    let nodes = joining
        .replayed
        .into_iter()
        .zip(joining.alive)
        .map(|(replayed, alive)| {
            let replayed = replayed.filter(|_| alive)?;
            Some(Node {
                statements: replayed.statements,
                exit: replayed.exit,
            })
        })
        .collect();
    Ok(Graph { nodes })
}

/// The blocks as they are replayed and joined.
struct Joining {
    replayed: Vec<Option<Replayed>>,
    alive: Vec<bool>,
    predecessors: Vec<Vec<usize>>,
}

impl Joining {
    fn replaced_predecessor(&mut self, block: usize, old: usize, new: usize) {
        let predecessors = &mut self.predecessors[block];
        predecessors.retain(|predecessor| *predecessor != old);
        if !predecessors.contains(&new) {
            predecessors.push(new);
        }
    }

    /// The stack `block` starts with: what its one replayed predecessor
    /// left — after two branches' values joined as `?:` — or nothing.
    fn entry(&mut self, block: usize) -> Result<Vec<Value>, Unsupported> {
        loop {
            let replayed: Vec<usize> = self.predecessors[block]
                .iter()
                .copied()
                .filter(|predecessor| self.replayed[*predecessor].is_some())
                .collect();
            let carrying = replayed.iter().any(|predecessor| {
                self.replayed[*predecessor]
                    .as_ref()
                    .is_some_and(|replayed| !replayed.stack.is_empty())
            });
            if !carrying {
                return Ok(Vec::new());
            }
            let only_one = replayed.len() == 1 && self.predecessors[block].len() == 1;
            if only_one {
                return Ok(self.replayed[replayed[0]]
                    .as_ref()
                    .map_or_else(Vec::new, |replayed| replayed.stack.clone()));
            }
            if !self.join_values(block, &replayed) {
                return Err(Unsupported::StackShape);
            }
        }
    }

    /// Two predecessors of `block` that only push a value each, on the two
    /// branches of one condition: that condition's block now pushes
    /// `condition ? value : value` and goes to `block`.
    fn join_values(&mut self, block: usize, candidates: &[usize]) -> bool {
        for first in candidates {
            for second in candidates {
                if first == second {
                    continue;
                }
                let Some(condition_block) = self.value_branch_origin(block, *first) else {
                    continue;
                };
                if self.value_branch_origin(block, *second) != Some(condition_block) {
                    continue;
                }
                let Some(Exit::Branch {
                    condition,
                    if_true,
                    if_false,
                }) = self.replayed[condition_block]
                    .as_ref()
                    .map(|replayed| replayed.exit.clone())
                else {
                    continue;
                };
                let pair = (if_true == *first && if_false == *second)
                    || (if_true == *second && if_false == *first);
                if !pair {
                    continue;
                }
                let value_of = |joining: &Joining, branch: usize| -> Option<Value> {
                    joining.replayed[branch]
                        .as_ref()
                        .and_then(|replayed| replayed.stack.last().cloned())
                };
                let (Some(when_true), Some(when_false)) =
                    (value_of(self, if_true), value_of(self, if_false))
                else {
                    continue;
                };
                // `c ? a : b` jumps to `b` when `!c`.
                let joined = Value {
                    expression: Expression::Conditional {
                        condition: Box::new(negated(condition)),
                        then: Box::new(when_false.expression),
                        otherwise: Box::new(when_true.expression),
                    },
                    kind: when_false.kind,
                };
                if let Some(replayed) = self.replayed[condition_block].as_mut() {
                    replayed.stack.push(joined);
                    replayed.exit = Exit::Goto(block);
                }
                self.alive[*first] = false;
                self.alive[*second] = false;
                self.predecessors[block]
                    .retain(|predecessor| predecessor != first && predecessor != second);
                self.predecessors[block].push(condition_block);
                return true;
            }
        }
        false
    }

    /// The block whose condition `branch` is one side of, if `branch` only
    /// pushes one value on top of what that block left and goes to `join`.
    fn value_branch_origin(&self, join: usize, branch: usize) -> Option<usize> {
        let replayed = self.replayed[branch].as_ref()?;
        let only_pushes = replayed.statements.is_empty() && replayed.exit == Exit::Goto(join);
        let [origin] = self.predecessors[branch][..] else {
            return None;
        };
        let origin_stack = &self.replayed[origin].as_ref()?.stack;
        let pushed_one = replayed.stack.len() == origin_stack.len() + 1
            && replayed.stack[..origin_stack.len()] == origin_stack[..];
        (only_pushes && pushed_one).then_some(origin)
    }

    /// `block` only tests a condition, reached from one block testing
    /// another that shares a target with it: the two are one test, `&&`
    /// or `||`.
    fn join_condition(&mut self, block: usize) {
        let Some(replayed) = self.replayed[block].as_ref() else {
            return;
        };
        let Exit::Branch {
            condition: second,
            if_true: second_true,
            if_false: second_false,
        } = replayed.exit.clone()
        else {
            return;
        };
        let [previous] = self.predecessors[block][..] else {
            return;
        };
        let Some(previous_replayed) = self.replayed[previous].as_ref() else {
            return;
        };
        let only_tests =
            replayed.statements.is_empty() && replayed.stack == previous_replayed.stack;
        let Exit::Branch {
            condition: first,
            if_true: first_true,
            if_false: first_false,
        } = previous_replayed.exit.clone()
        else {
            return;
        };
        if !only_tests || first_true == first_false {
            return;
        }
        let or = |left: Expression, right: Expression| Expression::Logical {
            operator: LogicalOperator::Or,
            left: Box::new(left),
            right: Box::new(right),
        };
        let and = |left: Expression, right: Expression| Expression::Logical {
            operator: LogicalOperator::And,
            left: Box::new(left),
            right: Box::new(right),
        };
        let joined = if first_false == block {
            if second_true == first_true {
                Some((or(first, second), first_true, second_false))
            } else if second_false == first_true {
                Some((or(first, negated(second)), first_true, second_true))
            } else {
                None
            }
        } else if first_true == block {
            if second_false == first_false {
                Some((and(first, second), second_true, first_false))
            } else if second_true == first_false {
                Some((and(first, negated(second)), second_false, first_false))
            } else {
                None
            }
        } else {
            None
        };
        let Some((condition, if_true, if_false)) = joined else {
            return;
        };
        if let Some(previous_replayed) = self.replayed[previous].as_mut() {
            previous_replayed.exit = Exit::Branch {
                condition,
                if_true,
                if_false,
            };
        }
        self.alive[block] = false;
        for successor in [second_true, second_false] {
            self.replaced_predecessor(successor, block, previous);
        }
    }
}

/// The blocks reachable from `entry`, each after all it is reached from
/// first (but by a jump back).
pub(super) fn reverse_postorder(successors: &[Vec<usize>], entry: usize) -> Vec<usize> {
    let mut visited = vec![false; successors.len()];
    let mut postorder = Vec::with_capacity(successors.len());
    let mut stack: Vec<(usize, usize)> = vec![(entry, 0)];
    visited[entry] = true;
    while let Some((node, next)) = stack.pop() {
        if let Some(successor) = successors[node].get(next).copied() {
            stack.push((node, next + 1));
            if !visited[successor] {
                visited[successor] = true;
                stack.push((successor, 0));
            }
        } else {
            postorder.push(node);
        }
    }
    postorder.reverse();
    postorder
}

/// Every instruction's offset, length and jumps.
fn decode(bytes: &[u8]) -> Result<Vec<Decoded>, Unsupported> {
    let mut decoded = Vec::new();
    let mut pc = 0;
    let byte = |at: usize| bytes.get(at).copied().ok_or(Unsupported::Malformed);
    let i32_at = |at: usize| -> Result<i32, Unsupported> {
        Ok(i32::from_be_bytes([
            byte(at)?,
            byte(at + 1)?,
            byte(at + 2)?,
            byte(at + 3)?,
        ]))
    };
    let target = |pc: usize, offset: i64| {
        usize::try_from(pc as i64 + offset).map_err(|_| Unsupported::Malformed)
    };
    while pc < bytes.len() {
        let opcode = byte(pc)?;
        let mut targets = Vec::new();
        let mut ends_flow = false;
        let length = match opcode {
            0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xbc => 2,
            0xa9 => return Err(Unsupported::Branches),
            0x11 | 0x13 | 0x14 | 0x84 | 0xb2..=0xb8 | 0xbb | 0xbd | 0xc0 | 0xc1 => 3,
            0x99..=0xa6 | 0xc6 | 0xc7 => {
                let offset = i16::from_be_bytes([byte(pc + 1)?, byte(pc + 2)?]);
                targets.push(target(pc, i64::from(offset))?);
                3
            }
            0xa7 => {
                let offset = i16::from_be_bytes([byte(pc + 1)?, byte(pc + 2)?]);
                targets.push(target(pc, i64::from(offset))?);
                ends_flow = true;
                3
            }
            0xc8 => {
                targets.push(target(pc, i64::from(i32_at(pc + 1)?))?);
                ends_flow = true;
                5
            }
            0xa8 | 0xc9 => return Err(Unsupported::Branches),
            0xc5 => 4,
            0xb9 | 0xba => 5,
            0xc4 => {
                if byte(pc + 1)? == 0x84 {
                    6
                } else {
                    4
                }
            }
            0xaa | 0xab => {
                let mut at = pc + 1;
                at += (4 - at % 4) % 4;
                targets.push(target(pc, i64::from(i32_at(at)?))?);
                ends_flow = true;
                let end = if opcode == 0xaa {
                    let low = i64::from(i32_at(at + 4)?);
                    let high = i64::from(i32_at(at + 8)?);
                    let count =
                        usize::try_from(high - low + 1).map_err(|_| Unsupported::Malformed)?;
                    for index in 0..count {
                        targets.push(target(pc, i64::from(i32_at(at + 12 + index * 4)?))?);
                    }
                    at + 12 + count * 4
                } else {
                    let pairs =
                        usize::try_from(i32_at(at + 4)?).map_err(|_| Unsupported::Malformed)?;
                    for index in 0..pairs {
                        targets.push(target(pc, i64::from(i32_at(at + 12 + index * 8)?))?);
                    }
                    at + 8 + pairs * 8
                };
                end - pc
            }
            0xac..=0xb1 | 0xbf => {
                ends_flow = true;
                1
            }
            _ => 1,
        };
        let ends_block = ends_flow || !targets.is_empty();
        decoded.push(Decoded {
            pc,
            length,
            targets,
            ends_flow,
            ends_block,
        });
        pc += length;
    }
    Ok(decoded)
}
