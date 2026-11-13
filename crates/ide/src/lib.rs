//! Editor features as pure functions over a database snapshot.
//!
//! [`AnalysisHost`] owns the database and takes changes; [`Analysis`] is a
//! snapshot that answers questions on any thread. When a change arrives
//! while a snapshot is still computing, that computation is cancelled and
//! its call returns `Err(Cancelled)` — the editor asks again.

mod document_symbols;
mod goto_definition;
mod hover;
mod navigation_target;
mod statistics;
mod workspace_symbols;

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use hir::{FileChange, FileId, ProjectModel, RootDatabase, SourceDatabase};
use syntax::{SyntaxError, TextRange};

pub use document_symbols::DocumentSymbol;
pub use hir::salsa::Cancelled;
pub use hover::HoverResult;
pub use navigation_target::{NavigationFile, NavigationTarget, SymbolKind};
pub use statistics::DeclarationStatistics;

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

    /// A snapshot to answer questions with, possibly on another thread.
    pub fn analysis(&self) -> Analysis {
        Analysis {
            db: self.db.clone(),
        }
    }
}

/// A read-only snapshot of the database.
pub struct Analysis {
    db: RootDatabase,
}

impl Analysis {
    fn with_db<T>(&self, query: impl FnOnce(&RootDatabase) -> T) -> Cancellable<T> {
        Cancelled::catch(AssertUnwindSafe(|| query(&self.db)))
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

    /// The declarations the name at `offset` refers to.
    pub fn goto_definition(
        &self,
        file: FileId,
        offset: usize,
    ) -> Cancellable<Vec<NavigationTarget>> {
        self.with_db(|db| goto_definition::goto_definition(db, file, offset))
    }

    /// Computes `files`' item trees so later queries find them cached.
    pub fn prime_item_trees(&self, files: &[FileId]) -> Cancellable<()> {
        self.with_db(|db| statistics::prime_item_trees(db, files))
    }

    /// Workspace-wide declaration and resolution counts.
    pub fn declaration_statistics(&self) -> Cancellable<DeclarationStatistics> {
        self.with_db(statistics::declaration_statistics)
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
            tree.items
                .iter()
                .find(|item| item.name_range.contains(offset))
                .map(|item| item.name_range)
        })
    }
}

#[cfg(test)]
mod tests;
