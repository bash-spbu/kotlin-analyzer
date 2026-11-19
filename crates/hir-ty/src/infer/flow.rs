//! What the data flow knows at a point of a body, as K2's data-flow
//! analysis keeps it: for each local or stable property a smart cast
//! narrowed, its type there — or that it can't be told — and whether any
//! way reaches the point at all. Every branching construct forks the
//! flow, runs each way on its own copy and merges the ways that go on:
//! a fact survives a merge as what all of them know (`Circle` and
//! `Square` give `Shape`), and a way that jumps (`return`, `throw`,
//! `break`) doesn't count. Lexical scopes only hold names: what's
//! narrowed outlives the block it was narrowed in, as it does in Kotlin.

use rustc_hash::FxHashMap;

use super::declarations::Inferrer;
use super::fit::Fit;
use super::narrowing::Narrowed;
use crate::ty::{Nullability, Ty};

/// What's known of a narrowed value's type at a point.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Fact {
    Known(Ty),
    /// Ways that disagree in a way no type tells (an assignment of a value
    /// whose type isn't known, two types without a common supertype
    /// known): the value is its declared type, but nothing is judged of it.
    Unsure,
}

#[derive(Clone, Default, Debug)]
pub(super) struct Flow {
    facts: FxHashMap<Narrowed, Fact>,
    /// No way reaches here: after a `return`, a `throw`, a `break`, a call
    /// that never returns.
    dead: bool,
}

impl Flow {
    pub(super) fn fact(&self, subject: Narrowed) -> Option<&Fact> {
        self.facts.get(&subject)
    }

    pub(super) fn is_dead(&self) -> bool {
        self.dead
    }

    /// No way goes on from here.
    pub(super) fn kill(&mut self) {
        self.dead = true;
    }

    fn set(&mut self, subject: Narrowed, fact: Fact) {
        self.facts.insert(subject, fact);
    }

    /// Back to the declared type, as known as declared.
    pub(super) fn forget(&mut self, subject: Narrowed) {
        self.facts.remove(&subject);
    }
}

impl Inferrer<'_> {
    /// A local's or a stable property's type here: as last narrowed, or as
    /// declared.
    pub(super) fn narrowed_type(&self, subject: Narrowed) -> Ty {
        match self.flow.fact(subject) {
            Some(Fact::Known(ty)) => ty.clone(),
            Some(Fact::Unsure) | None => self.declared_type_of(subject),
        }
    }

    /// Whether the ways reaching here disagree about `subject` in a way no
    /// type tells: what it is isn't judged.
    pub(super) fn is_unsure(&self, subject: Narrowed) -> bool {
        matches!(self.flow.fact(subject), Some(Fact::Unsure))
    }

    fn declared_type_of(&self, subject: Narrowed) -> Ty {
        match subject {
            Narrowed::Local(local) => self.local_declared_type(local),
            Narrowed::Property(path) => self.property_paths[path.0 as usize].ty.clone(),
        }
    }

    /// From here on, each subject has its type (and what its being not
    /// null vouches for, `val chain = info?.let { … }`).
    pub(super) fn narrow(&mut self, narrowings: &[(Narrowed, Ty)]) {
        let implied = self.implied_not_null(narrowings);
        for (subject, ty) in narrowings.iter().chain(implied.iter()) {
            self.flow.set(*subject, Fact::Known(ty.clone()));
        }
    }

    /// From here on, nothing is judged of `subject`.
    pub(super) fn make_unsure(&mut self, subject: Narrowed) {
        self.flow.set(subject, Fact::Unsure);
    }

    /// From here on, `subject` is as declared.
    pub(super) fn forget(&mut self, subject: Narrowed) {
        self.flow.forget(subject);
    }

    /// The end of one way of a branch whose value is `ty`: no way goes on
    /// when it never completes (`return`, `throw`, a call returning
    /// `Nothing`).
    pub(super) fn end_way(&mut self, ty: &Ty) {
        if matches!(ty, Ty::Nothing { nullable: false }) {
            self.flow.kill();
        }
    }

    /// A loop starts: `break` and `continue` in it (or naming its label,
    /// written before it: `outer@ for …`) go to it.
    pub(super) fn enter_loop(&mut self, node: syntax::SyntaxNode<'_>) {
        let label = syntax::previous_named_sibling(node)
            .filter(|sibling| sibling.kind() == "label")
            .map(|label| intern::Name::new(self.text(label).trim_matches('@')));
        self.loops.push(LoopFrame {
            label,
            ..LoopFrame::default()
        });
    }

    /// The loop ends: the ways that left it by `break` or went round by
    /// `continue`.
    pub(super) fn leave_loop(&mut self) -> LoopFrame {
        self.loops.pop().unwrap_or_default()
    }

    /// The loop a `break` or `continue` goes to: the one its label names,
    /// else the innermost.
    pub(super) fn loop_frame(&mut self, label: Option<intern::Name>) -> Option<&mut LoopFrame> {
        match label {
            Some(label) => self
                .loops
                .iter_mut()
                .rev()
                .find(|frame| frame.label == Some(label)),
            None => self.loops.last_mut(),
        }
    }

    /// Runs `run` on a copy of the flow here, gives the flow it ends with
    /// and puts the one from before back: one way of a branch.
    pub(super) fn fork<T>(&mut self, run: impl FnOnce(&mut Self) -> T) -> (T, Flow) {
        let before = self.flow.clone();
        let result = run(self);
        let end = std::mem::replace(&mut self.flow, before);
        (result, end)
    }

    /// What every way that goes on knows: per subject, the common supertype
    /// of its types on each way (its declared type where a way didn't
    /// narrow it), unsure where there's none known. Dead without any.
    pub(super) fn merge(&self, ways: Vec<Flow>) -> Flow {
        let mut live: Vec<Flow> = ways.into_iter().filter(|way| !way.dead).collect();
        if live.len() <= 1 {
            return live.pop().unwrap_or(Flow {
                dead: true,
                ..Flow::default()
            });
        }
        let mut subjects: Vec<Narrowed> = Vec::new();
        for way in &live {
            for subject in way.facts.keys() {
                if !subjects.contains(subject) {
                    subjects.push(*subject);
                }
            }
        }
        let mut merged = Flow::default();
        for subject in subjects {
            let declared = self.declared_type_of(subject);
            let mut joined: Option<Fact> = None;
            for way in &live {
                let fact = way
                    .fact(subject)
                    .cloned()
                    .unwrap_or_else(|| Fact::Known(declared.clone()));
                joined = Some(match joined {
                    None => fact,
                    Some(known) => self.join_facts(known, fact),
                });
            }
            match joined {
                Some(Fact::Known(ty)) if ty == declared => {}
                Some(fact) => merged.set(subject, fact),
                None => {}
            }
        }
        merged
    }

    /// What two ways' narrowings of the same subjects both say (`x is
    /// Circle || x is Square`: `x` is a `Shape`); a subject only one
    /// narrows is left as declared.
    pub(super) fn join_narrowings(
        &self,
        first: &[(Narrowed, Ty)],
        second: &[(Narrowed, Ty)],
    ) -> Vec<(Narrowed, Ty)> {
        let latest = |narrowings: &[(Narrowed, Ty)], subject: Narrowed| {
            narrowings
                .iter()
                .rev()
                .find(|(narrowed, _)| *narrowed == subject)
                .map(|(_, ty)| ty.clone())
        };
        let mut joined: Vec<(Narrowed, Ty)> = Vec::new();
        for (subject, _) in first {
            if joined.iter().any(|(known, _)| known == subject) {
                continue;
            }
            let (Some(one), Some(other)) = (latest(first, *subject), latest(second, *subject))
            else {
                continue;
            };
            if let Fact::Known(ty) = self.join_facts(Fact::Known(one), Fact::Known(other)) {
                joined.push((*subject, ty));
            }
        }
        joined
    }

    /// One type both facts allow: of the classes, the wider of two that
    /// fit one another, else their common supertype; of the nullabilities,
    /// the widest (`String` and a Java `String!` give `String!`). Unsure
    /// when no type is known.
    fn join_facts(&self, first: Fact, second: Fact) -> Fact {
        let (Fact::Known(one), Fact::Known(other)) = (first, second) else {
            return Fact::Unsure;
        };
        if one == other {
            return Fact::Known(one);
        }
        let nullability = widest(one.nullability(), other.nullability());
        let (one, other) = (one.non_null(), other.non_null());
        let base = if one == other || self.types.fits(&other, &one) == Fit::Yes {
            one
        } else if self.types.fits(&one, &other) == Fit::Yes {
            other
        } else {
            // A common supertype is only as sure as its arguments:
            // `List<String>` and `List<Int>` aren't both a `List<Int>`.
            let without_arguments =
                |ty: &Ty| ty.class().is_none_or(|class| class.arguments.is_empty());
            if !without_arguments(&one) || !without_arguments(&other) {
                return Fact::Unsure;
            }
            match self.types.common_type(&[one, other]) {
                Ty::Unknown => return Fact::Unsure,
                common => common,
            }
        };
        Fact::Known(base.with_nullability(nullability))
    }
}

/// A loop being inferred: its label, and the ways that leave it by
/// `break` and go round again by `continue`.
#[derive(Default)]
pub(super) struct LoopFrame {
    pub(super) label: Option<intern::Name>,
    pub(super) breaks: Vec<Flow>,
    pub(super) continues: Vec<Flow>,
}

/// The nullability that allows the most: nullable, then a Java type's
/// unknown one, then not null.
fn widest(first: Nullability, second: Nullability) -> Nullability {
    let rank = |nullability: Nullability| match nullability {
        Nullability::NotNull => 0,
        Nullability::Platform => 1,
        Nullability::Nullable => 2,
    };
    if rank(first) >= rank(second) {
        first
    } else {
        second
    }
}
