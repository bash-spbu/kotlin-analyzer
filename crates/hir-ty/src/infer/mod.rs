//! Inference of bodies: which declaration each name in a body means and
//! what type each expression has.
//!
//! One walk over a file's syntax tree, keeping a stack of scopes (locals,
//! and the smart casts in force) and a stack of implicit receivers (`this`
//! of the classes and receiver lambdas around). Calls are resolved against
//! the candidates Kotlin's scope levels give — locals, members of implicit
//! receivers, extensions, constructors, top-level functions — choosing
//! among overloads by the arguments' types, and inferring the callee's type
//! parameters from its receiver, its arguments and the expected type.
//! Lambdas get their parameters' types from the parameter they're passed
//! to.
//!
//! Whatever can't be known stays unknown: a name with no fitting candidate
//! keeps its candidates (or none) and its type is `Unknown`, so checks built
//! on this stay silent instead of guessing.

mod call;
mod completion;
mod declarations;
mod exhaustiveness;
mod expressions;
mod fit;
mod member_scope;
mod narrowing;

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::{ItemId, ItemLocation};
use intern::Name;
use rustc_hash::FxHashMap;
use syntax::{Language, TextRange};

use crate::ty::Ty;

pub use completion::{CompletionScope, PLACEHOLDER, Qualifier, completion_scope};
pub use fit::Fit;

/// A local declaration: a `val`/`var` in a body, a parameter, a lambda's
/// parameter (`it` included), a local function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Local {
    pub name: Name,
    /// For `it`, the lambda's opening brace.
    pub name_range: TextRange,
    pub ty: Ty,
    pub kind: LocalKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum LocalKind {
    Value,
    Variable,
    Parameter,
    LambdaParameter,
    /// A lambda's implicit single parameter.
    ImplicitIt,
    Function,
    /// `field` in a property's accessor: the value it stores.
    BackingField,
}

impl LocalKind {
    /// Whether the source spells the local's declaration (`it` and `field`
    /// are implicit: their name range is where they come into scope).
    pub fn is_written(self) -> bool {
        !matches!(self, LocalKind::ImplicitIt | LocalKind::BackingField)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LocalId(pub u32);

/// What a name in a body means.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reference {
    Local(LocalId),
    Declaration(ItemLocation),
    /// Several overloads fit equally, or none fits: the candidates that
    /// have the name.
    Candidates(Box<[ItemLocation]>),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedName {
    pub reference: Reference,
    /// The type of the value the name gives: a property's, or a call's
    /// result.
    pub ty: Ty,
}

/// A name in a body nothing was found for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnresolvedName {
    pub range: TextRange,
    pub name: Name,
    pub site: NameSite,
    pub certainty: Certainty,
}

/// Where a name was looked up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NameSite {
    /// On its own (`total`, `format(…)`): in the scope levels.
    Scope,
    /// After `receiver.`: among the members and extensions of its type.
    Member(Ty),
    /// After a class name (`Money.ZERO`): statics, nested classes, the
    /// companion.
    Static,
    /// After a package (`java.time.Instant`).
    Package,
}

/// Whether everything a lookup had to search was modelled completely: only
/// then is "nothing has this name" (or "no candidate takes these
/// arguments") a fact a diagnostic may report.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Certainty {
    Certain,
    /// Something in scope is unknown: a receiver of unknown type, a lambda
    /// that may have a receiver, a class whose supertypes don't all
    /// resolve, a declaration a compiler plugin generates.
    Uncertain,
}

/// A call whose candidates exist but none takes its arguments: too many or
/// too few, or a named argument no parameter has.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ArgumentMismatch {
    /// The callee's name.
    pub range: TextRange,
    pub candidates: Box<[ItemLocation]>,
}

/// `receiver.member` with a plain `.` where the receiver may be null.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnsafeCall {
    /// The member's name.
    pub range: TextRange,
    pub receiver: Ty,
}

/// A `when` over an enum, a sealed type or a `Boolean` without `else` that
/// misses cases.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NonExhaustiveWhen {
    /// The `when` keyword.
    pub range: TextRange,
    /// What's missing, as Kotlin would write it (`RED`, `is Missing`,
    /// `false`, `null`).
    pub missing: Vec<String>,
}

/// What inference found in one file.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct InferenceResult {
    /// By the start of the name.
    names: FxHashMap<u32, ResolvedName>,
    locals: Vec<Local>,
    /// By the start of the declared name.
    local_declarations: FxHashMap<u32, LocalId>,
    unresolved: Vec<UnresolvedName>,
    argument_mismatches: Vec<ArgumentMismatch>,
    unsafe_calls: Vec<UnsafeCall>,
    non_exhaustive_whens: Vec<NonExhaustiveWhen>,
}

impl InferenceResult {
    /// What the name starting at `offset` means.
    pub fn name_at(&self, offset: u32) -> Option<&ResolvedName> {
        self.names.get(&offset)
    }

    pub fn local(&self, local: LocalId) -> &Local {
        &self.locals[local.0 as usize]
    }

    /// The local whose declared name starts at `offset`.
    pub fn local_declared_at(&self, offset: u32) -> Option<LocalId> {
        self.local_declarations.get(&offset).copied()
    }

    pub fn locals(&self) -> impl Iterator<Item = (LocalId, &Local)> {
        self.locals
            .iter()
            .enumerate()
            .map(|(index, local)| (LocalId(index as u32), local))
    }

    /// Every resolved name: its start and what it means.
    pub fn names(&self) -> impl Iterator<Item = (u32, &ResolvedName)> {
        self.names.iter().map(|(start, name)| (*start, name))
    }

    pub fn unresolved(&self) -> &[UnresolvedName] {
        &self.unresolved
    }

    pub fn argument_mismatches(&self) -> &[ArgumentMismatch] {
        &self.argument_mismatches
    }

    pub fn unsafe_calls(&self) -> &[UnsafeCall] {
        &self.unsafe_calls
    }

    pub fn non_exhaustive_whens(&self) -> &[NonExhaustiveWhen] {
        &self.non_exhaustive_whens
    }
}

/// Infers every body of `file`.
#[salsa::tracked(returns(clone), lru = 64)]
pub fn infer_file(db: &dyn SourceDatabase, file: SourceFile) -> Arc<InferenceResult> {
    if file.language(db) != Language::Kotlin {
        return Arc::new(InferenceResult::default());
    }
    let parse = base_db::parse(db, file);
    let mut inferrer = declarations::Inferrer::new(db, file, &parse);
    inferrer.file_declarations();
    Arc::new(inferrer.finish())
}

/// The type of a workspace declaration that doesn't write one: a
/// function's expression body's (`Unit` for a block body), a property's
/// initializer's, getter's or delegate's. A declaration whose type depends
/// on itself is `Unknown`.
#[salsa::tracked(returns(clone), cycle_result = unknown_declaration_type, lru = 4096)]
pub fn declaration_type(db: &dyn SourceDatabase, file: SourceFile, item: ItemId) -> Ty {
    if file.language(db) != Language::Kotlin {
        return Ty::Unknown;
    }
    let range = hir_def::file_item_source_map(db, file).ranges(item).range;
    let parse = base_db::parse(db, file);
    let mut inferrer = declarations::Inferrer::new(db, file, &parse);
    inferrer.declaration_type(range)
}

fn unknown_declaration_type(
    _db: &dyn SourceDatabase,
    _id: salsa::Id,
    _file: SourceFile,
    _item: ItemId,
) -> Ty {
    Ty::Unknown
}
