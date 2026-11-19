//! Editor features as pure functions over a database snapshot.
//!
//! [`AnalysisHost`] owns the database and takes changes; [`Analysis`] is a
//! snapshot that answers questions on any thread. When a change arrives
//! while a snapshot is still computing, that computation is cancelled and
//! its call returns `Err(Cancelled)` — the editor asks again.

mod code_lens;
mod completion;
mod diagnostics;
mod doc_markdown;
mod document_symbols;
mod file_rename;
mod folding;
mod goto_definition;
mod goto_implementation;
mod goto_type_definition;
mod hierarchy;
mod hover;
mod inlay_hints;
mod navigation_target;
mod references;
mod selection_range;
mod semantic_tokens;
mod signature_help;
mod statistics;
mod workspace_symbols;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use hir::{FileChange, FileId, ProjectModel, RootDatabase, SourceDatabase};
use syntax::{SyntaxError, TextRange};

pub use code_lens::{CodeLens, CodeLensKind, TestTarget};
pub use completion::{CompletionItem, CompletionKind, Completions};
pub use diagnostics::{Check, Diagnostic, DiagnosticsConfig, Fix, Severity, Stability, TextEdit};
pub use document_symbols::DocumentSymbol;
pub use file_rename::FileMove;
pub use folding::{Fold, FoldKind};
pub use hierarchy::{HierarchyCall, HierarchyItem, HierarchyKey};
pub use hir::salsa::Cancelled;
pub use hover::HoverResult;
pub use inlay_hints::{HintTarget, InlayHint, InlayHintPart};
pub use navigation_target::{NavigationFile, NavigationTarget, SymbolKind};
pub use references::{Access, FileRange, Highlight, RenameError, SourceChange};
pub use semantic_tokens::{SemanticToken, TokenKind, TokenModifiers};
pub use signature_help::{SignatureHelp, SignatureInformation};
pub use statistics::{BodyStatistics, DeclarationStatistics, UnsettledName};

/// A result that a newer change may have cancelled.
pub type Cancellable<T> = Result<T, Cancelled>;

/// Owns the database; applies changes.
#[derive(Default)]
pub struct AnalysisHost {
    db: RootDatabase,
}

impl AnalysisHost {
    pub fn set_project_model(&mut self, model: Arc<ProjectModel>) {
        self.db.set_project_model(model);
    }

    pub fn apply_change(&mut self, changes: Vec<FileChange>) {
        self.db.apply_change(changes);
    }

    /// Sets the indexed libraries, which of them each source set sees, and
    /// which one is the JDK.
    pub fn set_libraries(
        &mut self,
        libraries: Vec<(hir::LibrarySource, hir::IndexedLibrary)>,
        by_source_set: Vec<Vec<usize>>,
        jdk: Option<usize>,
    ) {
        self.db.set_libraries(libraries, by_source_set, jdk);
    }

    /// Drops results beyond each query's limit. Salsa does so only between
    /// revisions on its own; a pass over every file calls this between
    /// batches so it doesn't keep all files' results. Waits for snapshots
    /// to be dropped.
    pub fn trigger_lru_eviction(&mut self) {
        use hir::salsa::Database;
        self.db.trigger_lru_eviction();
    }

    /// A snapshot to answer questions with, possibly on another thread.
    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: self.db.clone(),
        }
    }
}

/// A read-only snapshot of the database.
#[derive(Clone)]
pub struct Analysis {
    db: RootDatabase,
}

impl Analysis {
    fn with_db<T>(&self, query: impl FnOnce(&RootDatabase) -> T) -> Cancellable<T> {
        Cancelled::catch(AssertUnwindSafe(|| query(&self.db)))
    }

    /// Every file of the workspace's source sets.
    pub fn workspace_files(&self) -> Cancellable<Vec<FileId>> {
        self.with_db(|db| {
            db.all_files()
                .into_iter()
                .map(|file| file.file_id(db))
                .collect()
        })
    }

    pub fn file_text(&self, file: FileId) -> Cancellable<Option<Arc<str>>> {
        self.with_db(|db| db.source_file(file).map(|source| source.text(db).clone()))
    }

    /// Where the parser couldn't make sense of the file.
    pub fn syntax_errors(&self, file: FileId) -> Cancellable<Vec<SyntaxError>> {
        self.with_db(|db| match db.source_file(file) {
            Some(source) => hir::parse(db, source).errors(),
            None => Vec::new(),
        })
    }

    /// What can be written at `offset`.
    pub fn completions(&self, file: FileId, offset: usize) -> Cancellable<Completions> {
        self.with_db(|db| completion::completions(db, file, offset))
    }

    /// Every place the name at `offset` refers to the same thing.
    pub fn references(
        &self,
        file: FileId,
        offset: usize,
        include_declaration: bool,
    ) -> Cancellable<Vec<FileRange>> {
        self.with_db(|db| references::references(db, file, offset, include_declaration))
    }

    /// The occurrences in `file` of what the name at `offset` means, writes
    /// told apart.
    pub fn highlights(&self, file: FileId, offset: usize) -> Cancellable<Vec<Highlight>> {
        self.with_db(|db| references::highlights(db, file, offset))
    }

    /// The range of the name at `offset` when it can be renamed.
    /// What renaming or moving `moves`' files changes in the code (see
    /// `file_rename`), as edits to the files before they move.
    pub fn will_rename_files(&self, moves: &[FileMove]) -> Cancellable<SourceChange> {
        self.with_db(|db| file_rename::will_rename_files(db, moves))
    }

    pub fn prepare_rename(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Result<TextRange, RenameError>> {
        self.with_db(|db| references::prepare_rename(db, file, offset))
    }

    /// The edits renaming the name at `offset` everywhere.
    pub fn rename(
        &self,
        file: FileId,
        offset: usize,
        new_name: &str,
    ) -> Cancellable<Result<SourceChange, RenameError>> {
        self.with_db(|db| references::rename(db, file, offset, new_name))
    }

    /// What each name in `file` is, for colouring.
    pub fn semantic_tokens(&self, file: FileId) -> Cancellable<Vec<SemanticToken>> {
        self.with_db(|db| semantic_tokens::semantic_tokens(db, file))
    }

    /// The regions of `file` an editor can fold.
    pub fn folding_ranges(&self, file: FileId) -> Cancellable<Vec<Fold>> {
        self.with_db(|db| folding::folding_ranges(db, file))
    }

    /// For each offset, the ranges "expand selection" grows through, from
    /// the innermost out.
    pub fn selection_ranges(
        &self,
        file: FileId,
        offsets: &[usize],
    ) -> Cancellable<Vec<Vec<TextRange>>> {
        self.with_db(|db| selection_range::selection_ranges(db, file, offsets))
    }

    /// The lenses over `file`'s declarations: reference counts to resolve,
    /// test runs.
    pub fn code_lenses(&self, file: FileId) -> Cancellable<Vec<CodeLens>> {
        self.with_db(|db| code_lens::code_lenses(db, file))
    }

    /// The signatures of the call around `offset`.
    pub fn signature_help(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Option<SignatureHelp>> {
        self.with_db(|db| signature_help::signature_help(db, file, offset))
    }

    /// The diagnostics of `file` the configuration enables.
    pub fn diagnostics(
        &self,
        file: FileId,
        config: &DiagnosticsConfig,
    ) -> Cancellable<Vec<Diagnostic>> {
        self.with_db(|db| diagnostics::diagnostics(db, file, config))
    }

    /// The file's declarations as a tree.
    pub fn document_symbols(&self, file: FileId) -> Cancellable<Vec<DocumentSymbol>> {
        self.with_db(|db| document_symbols::document_symbols(db, file))
    }

    /// Workspace declarations whose names match `query`.
    pub fn workspace_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| workspace_symbols::workspace_symbols(db, query, limit))
    }

    /// What the name at `offset` is: its signature, documentation, place.
    pub fn hover(&self, file: FileId, offset: usize) -> Cancellable<Option<HoverResult>> {
        self.with_db(|db| hover::hover(db, file, offset))
    }

    /// Inferred types of the locals in `range` (the whole file when
    /// `None`) that don't write one.
    pub fn inlay_hints(
        &self,
        file: FileId,
        range: Option<TextRange>,
    ) -> Cancellable<Vec<InlayHint>> {
        self.with_db(|db| inlay_hints::inlay_hints(db, file, range))
    }

    /// Where each part of the hint at `offset` leads (see
    /// [`HintTarget::LibraryDeclaration`]).
    pub fn resolve_inlay_hint(
        &self,
        file: FileId,
        offset: u32,
    ) -> Cancellable<Vec<Option<NavigationTarget>>> {
        self.with_db(|db| inlay_hints::resolve_inlay_hint(db, file, offset))
    }

    /// The declarations the name at `offset` refers to.
    pub fn goto_definition(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| goto_definition::goto_definition(db, file, offset))
    }

    /// What the completion item `label` offered at `offset` names, as
    /// `completionItem/resolve` documents it: its whole signature, its
    /// documentation, where it's declared.
    pub fn completion_documentation(
        &self,
        file: FileId,
        offset: usize,
        label: &str,
    ) -> Cancellable<Option<String>> {
        self.with_db(|db| completion::documentation(db, file, offset, label))
    }

    /// The classes of the type of what's named at `offset`.
    pub fn goto_type_definition(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| goto_type_definition::goto_type_definition(db, file, offset))
    }

    /// What implements the declaration named at `offset`: a class's
    /// subclasses, a member's overrides, an `expect`'s `actual`s.
    /// Where the name at `offset` is declared for every platform: an
    /// `actual`'s `expect`, else its definition.
    pub fn goto_declaration(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| goto_definition::goto_declaration(db, file, offset))
    }

    /// The class at `offset`, to show the hierarchy of.
    pub fn prepare_type_hierarchy(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<HierarchyItem>> {
        self.with_db(|db| hierarchy::prepare_type_hierarchy(db, file, offset))
    }

    /// A class's direct supertypes (`key` resolved from `from`).
    pub fn supertypes(&self, from: FileId, key: &HierarchyKey) -> Cancellable<Vec<HierarchyItem>> {
        self.with_db(|db| hierarchy::supertypes(db, from, key))
    }

    /// The workspace classes extending a class directly.
    pub fn subtypes(&self, from: FileId, key: &HierarchyKey) -> Cancellable<Vec<HierarchyItem>> {
        self.with_db(|db| hierarchy::subtypes(db, from, key))
    }

    /// The function at `offset`, to show the calls of.
    pub fn prepare_call_hierarchy(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<HierarchyItem>> {
        self.with_db(|db| hierarchy::prepare_call_hierarchy(db, file, offset))
    }

    /// Who calls a function, grouped by caller.
    pub fn incoming_calls(&self, key: &HierarchyKey) -> Cancellable<Vec<HierarchyCall>> {
        self.with_db(|db| hierarchy::incoming_calls(db, key))
    }

    /// What a function calls, grouped by callee.
    pub fn outgoing_calls(&self, key: &HierarchyKey) -> Cancellable<Vec<HierarchyCall>> {
        self.with_db(|db| hierarchy::outgoing_calls(db, key))
    }

    pub fn goto_implementation(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| goto_implementation::goto_implementation(db, file, offset))
    }

    /// Computes `files`' item trees so later queries find them cached.
    pub fn prime_item_trees(&self, files: &[FileId]) -> Cancellable<()> {
        self.with_db(|db| statistics::prime_item_trees(db, files))
    }

    /// Workspace-wide declaration and resolution counts.
    pub fn declaration_statistics(&self) -> Cancellable<DeclarationStatistics> {
        self.with_db(statistics::declaration_statistics)
    }

    /// What inference resolves in the bodies of `files`.
    pub fn body_statistics(&self, files: &[FileId]) -> Cancellable<BodyStatistics> {
        self.with_db(|db| statistics::body_statistics(db, files))
    }

    /// The names in `file`'s bodies inference left unresolved or with
    /// several candidates.
    pub fn unsettled_names(&self, file: FileId) -> Cancellable<Vec<UnsettledName>> {
        self.with_db(|db| statistics::unsettled_names(db, file))
    }

    /// The range of the declaration name at `offset`, when there is one.
    pub fn declaration_name_at(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Option<TextRange>> {
        self.with_db(|db| {
            let source = db.source_file(file)?;
            let tree = hir::file_item_tree(db, source);
            let source_map = hir::file_item_source_map(db, source);
            tree.indexed_items()
                .map(|(item, _)| source_map.ranges(item).name_range)
                .find(|name_range| name_range.contains(offset))
        })
    }
}

#[cfg(test)]
mod tests;
