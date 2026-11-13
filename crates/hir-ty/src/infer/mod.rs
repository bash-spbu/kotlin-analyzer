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
mod declarations;
mod expressions;
mod fit;
mod narrowing;

use std::sync::Arc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::{ItemIndex, ItemLocation, file_item_tree};
use intern::Name;
use rustc_hash::FxHashMap;
use syntax::{Language, TextRange};

use crate::ty::Ty;

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

/// What inference found in one file.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct InferenceResult {
    /// By the start of the name.
    names: FxHashMap<u32, ResolvedName>,
    locals: Vec<Local>,
    /// By the start of the declared name.
    local_declarations: FxHashMap<u32, LocalId>,
    /// Names in expressions that nothing was found for.
    unresolved: Vec<(u32, Name)>,
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

    pub fn unresolved(&self) -> &[(u32, Name)] {
        &self.unresolved
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
#[salsa::tracked(returns(clone), cycle_result = unknown_declaration_type)]
pub fn declaration_type(db: &dyn SourceDatabase, file: SourceFile, item: ItemIndex) -> Ty {
    if file.language(db) != Language::Kotlin {
        return Ty::Unknown;
    }
    let tree = file_item_tree(db, file);
    let range = tree.item(item).range;
    let parse = base_db::parse(db, file);
    let mut inferrer = declarations::Inferrer::new(db, file, &parse);
    inferrer.declaration_type(range)
}

fn unknown_declaration_type(
    _db: &dyn SourceDatabase,
    _id: salsa::Id,
    _file: SourceFile,
    _item: ItemIndex,
) -> Ty {
    Ty::Unknown
}
