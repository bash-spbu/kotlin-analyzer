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
mod contracts;
mod declarations;
mod exhaustiveness;
mod expressions;
mod fit;
mod local_usage;
mod member_scope;
mod narrowing;
mod property_paths;
mod reassociation;
mod type_mismatches;
mod type_operations;
mod when_branches;

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::{ItemId, ItemLocation};
use intern::Name;
use rustc_hash::FxHashMap;
use syntax::{Language, TextRange};

use crate::ty::Ty;

pub use completion::{CompletionScope, PLACEHOLDER, Qualifier, completion_scope};
pub use fit::Fit;
pub use local_usage::{LocalFinding, local_findings};

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
    /// A `val`/`var` parameter of a primary constructor: one declaration
    /// that is the parameter in the class's initializers and defaults, and
    /// the class's property of the same name everywhere else.
    PropertyParameter,
    LambdaParameter,
    /// A lambda's implicit single parameter.
    ImplicitIt,
    Function,
    /// `field` in a property's accessor: the value it stores.
    BackingField,
    /// A class declared in a body (`data class Case(…)` in a test): its
    /// name as a value (`Case(…)`, `::Case`) means it, not a namesake
    /// outside. Its members aren't modelled.
    Class,
}

impl LocalKind {
    /// Whether the source spells the local's declaration (`it` and `field`
    /// are implicit: their name range is where they come into scope).
    pub fn is_written(self) -> bool {
        !matches!(self, LocalKind::ImplicitIt | LocalKind::BackingField)
    }

    /// Whether every use of the local is recorded: a local class's uses as
    /// a type aren't, so renaming it would miss them.
    pub fn uses_are_all_recorded(self) -> bool {
        self != LocalKind::Class
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

/// A null check on a value whose type isn't nullable: after smart casts
/// for a value that can't change, as declared for a `var` (Kotlin doesn't
/// smart-cast every `var` our data flow narrows).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RedundantNullCheck {
    pub kind: NullCheckKind,
    /// The operator (`!!`, `?.`, `?:`), or the whole comparison.
    pub range: TextRange,
    /// The value's type.
    pub ty: Ty,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NullCheckKind {
    /// `x!!`.
    NotNullAssertion,
    /// `x?.f`.
    SafeCall,
    /// `x ?: y`; `removable` spans ` ?: y`.
    Elvis { removable: TextRange },
    /// `x == null` (always `false`) or `x != null` (always `true`).
    Comparison { always: bool },
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

/// A cast or type check whose outcome the types decide.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeOperationFinding {
    pub kind: TypeOperationKind,
    /// ` as T` for a useless or unchecked cast, `as` for one that never
    /// succeeds, the whole check for `is`.
    pub range: TextRange,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypeOperationKind {
    /// `x as T` where `x` is a `T` already; `removable` spans ` as T`.
    UselessCast { removable: TextRange },
    /// `x as T` where no value of `x`'s type is a `T`.
    CastNeverSucceeds,
    /// `x as T` the run time can't check: `T` is a type parameter, or has
    /// type arguments `x`'s type doesn't determine.
    UncheckedCast { from: Ty, to: Ty },
    /// `x is T` (`result` true) or `x !is T` (false) where `x` is a `T`.
    UselessIsCheck { result: bool },
}

/// A value whose class can't be the one where it goes requires.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeMismatch {
    pub site: MismatchSite,
    /// The value — narrowed to the member name of a call or property
    /// chain.
    pub range: TextRange,
    pub expected: Ty,
    pub actual: Ty,
}

/// Where a value goes that requires a type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MismatchSite {
    /// `return x`, `fun f(): T = x`.
    Return,
    /// `val x: T = value`, and a parameter's default value.
    Initializer,
    /// `x = value` to a `var` that writes its type.
    Assignment,
    /// `f(value)`, the only function the call can mean.
    Argument,
}

/// What a `when` with a subject has twice or needlessly.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WhenFinding {
    /// An `else` after branches covering every case: its keyword, and the
    /// whole branch.
    RedundantElse {
        keyword: TextRange,
        entry: TextRange,
    },
    /// A condition an earlier branch already checks.
    DuplicateCondition { range: TextRange },
}

impl WhenFinding {
    pub fn range(&self) -> TextRange {
        match self {
            WhenFinding::RedundantElse { keyword, .. } => *keyword,
            WhenFinding::DuplicateCondition { range } => *range,
        }
    }
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
    redundant_null_checks: Vec<RedundantNullCheck>,
    /// The names of `val`s, parameters and loop variables assigned again
    /// (`VAL_REASSIGNMENT`).
    val_reassignments: Vec<TextRange>,
    /// The closing braces of block-bodied functions returning a value whose
    /// end is reachable (`NO_RETURN_IN_FUNCTION_WITH_BLOCK_BODY`).
    missing_returns: Vec<TextRange>,
    /// By the start of the cast's `as` or the check; `None` once examined
    /// and found fine (a lambda inferred again overwrites it).
    type_operations: FxHashMap<u32, Option<TypeOperationFinding>>,
    /// By the start of the `when`; a lambda inferred again overwrites them.
    when_findings: FxHashMap<u32, Vec<WhenFinding>>,
    /// By the start of the value; `None` once examined and fine.
    type_mismatches: FxHashMap<u32, Option<TypeMismatch>>,
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

    pub fn redundant_null_checks(&self) -> &[RedundantNullCheck] {
        &self.redundant_null_checks
    }

    pub fn val_reassignments(&self) -> &[TextRange] {
        &self.val_reassignments
    }

    pub fn missing_returns(&self) -> &[TextRange] {
        &self.missing_returns
    }

    /// Values whose class can't be the one required where they go, in
    /// text order.
    pub fn type_mismatches(&self) -> Vec<&TypeMismatch> {
        let mut found: Vec<&TypeMismatch> = self.type_mismatches.values().flatten().collect();
        found.sort_by_key(|mismatch| mismatch.range.start);
        found
    }

    /// What `when`s have twice or needlessly, in text order.
    pub fn when_findings(&self) -> Vec<&WhenFinding> {
        let mut found: Vec<&WhenFinding> = self.when_findings.values().flatten().collect();
        found.sort_by_key(|finding| finding.range().start);
        found
    }

    /// Casts and type checks whose outcome is known, in text order.
    pub fn type_operations(&self) -> Vec<&TypeOperationFinding> {
        let mut found: Vec<&TypeOperationFinding> =
            self.type_operations.values().flatten().collect();
        found.sort_by_key(|finding| finding.range.start);
        found
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
