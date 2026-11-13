//! The incremental database's inputs — file texts, the project model, which
//! files make up each source set — and the one query everything else starts
//! from: parsing a file. Every derived fact is a query over these, so an
//! edit invalidates exactly what depends on it.

pub mod fixture;

use std::sync::Arc;

use rustc_hash::FxHashMap;

pub use project_model::{ProjectModel, SourceSetId};
pub use salsa;
pub use syntax::{Language, Parse};
pub use vfs::FileId;

/// One source file's input: its text and where it belongs.
#[salsa::input(debug)]
pub struct SourceFile {
    #[returns(copy)]
    pub file_id: FileId,
    #[returns(ref)]
    pub text: Arc<str>,
    #[returns(copy)]
    pub language: Language,
    /// `None` for a file outside every source set (opened by itself).
    #[returns(copy)]
    pub source_set: Option<SourceSetId>,
}

/// One source set's files — the key of per-source-set queries.
#[salsa::input(debug)]
pub struct SourceSet {
    #[returns(copy)]
    pub id: SourceSetId,
    #[returns(ref)]
    pub files: Arc<[SourceFile]>,
}

/// The workspace as a whole.
#[salsa::input(debug)]
pub struct Workspace {
    #[returns(ref)]
    pub model: Arc<ProjectModel>,
    /// Indexed by `SourceSetId`.
    #[returns(ref)]
    pub source_sets: Arc<[SourceSet]>,
}

/// What every query can ask the database for besides its inputs' fields.
#[salsa::db]
pub trait SourceDatabase: salsa::Database {
    fn workspace(&self) -> Workspace;
    fn source_file(&self, file: FileId) -> Option<SourceFile>;
}

/// The syntax tree of a file. Kept for the files used recently; any other is
/// parsed again on demand.
#[salsa::tracked(returns(ref), lru = 256)]
pub fn parse(db: &dyn SourceDatabase, file: SourceFile) -> Parse {
    syntax::parse(file.text(db).clone(), file.language(db))
}

/// The database: Salsa's storage plus the handles of its inputs. Cloning it
/// makes a snapshot that reads while the original waits to write.
#[salsa::db]
#[derive(Clone)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
    workspace: Option<Workspace>,
    files: Arc<FxHashMap<FileId, SourceFile>>,
}

impl Default for RootDatabase {
    fn default() -> Self {
        Self {
            storage: salsa::Storage::new(None),
            workspace: None,
            files: Arc::default(),
        }
    }
}

#[salsa::db]
impl salsa::Database for RootDatabase {}

#[salsa::db]
impl SourceDatabase for RootDatabase {
    fn workspace(&self) -> Workspace {
        self.workspace
            .expect("a workspace is set before the database is queried")
    }

    fn source_file(&self, file: FileId) -> Option<SourceFile> {
        self.files.get(&file).copied()
    }
}

/// A file's new state for [`RootDatabase::apply_change`].
pub struct FileChange {
    pub file: FileId,
    /// `None`: deleted.
    pub text: Option<Arc<str>>,
    pub language: Language,
    pub source_set: Option<SourceSetId>,
}

impl RootDatabase {
    /// Sets the project model, keeping existing files' inputs.
    pub fn set_project_model(&mut self, model: Arc<ProjectModel>) {
        let source_sets = self.source_set_inputs(&model);
        match self.workspace {
            Some(workspace) => {
                use salsa::Setter;
                workspace.set_model(self).to(model);
                workspace.set_source_sets(self).to(source_sets);
            }
            None => self.workspace = Some(Workspace::new(self, model, source_sets)),
        }
    }

    /// One input per source set of `model`, holding the files already
    /// known to belong to it.
    fn source_set_inputs(&self, model: &ProjectModel) -> Arc<[SourceSet]> {
        let mut grouped: Vec<Vec<SourceFile>> = vec![Vec::new(); model.source_sets.len()];
        for file in self.files.values() {
            let group = file
                .source_set(self)
                .and_then(|id| grouped.get_mut(id.0 as usize));
            if let Some(group) = group {
                group.push(*file);
            }
        }
        grouped
            .into_iter()
            .enumerate()
            .map(|(index, mut files)| {
                files.sort_by_key(|file| file.file_id(self));
                SourceSet::new(self, SourceSetId(index as u32), files.into())
            })
            .collect()
    }

    /// Applies new texts. Source sets whose file lists change get new lists;
    /// a file whose text alone changed only invalidates what read it.
    pub fn apply_change(&mut self, changes: Vec<FileChange>) {
        use salsa::Setter;
        let mut membership_changed: Vec<SourceSetId> = Vec::new();
        for change in changes {
            let existing = self.files.get(&change.file).copied();
            match (existing, change.text) {
                (Some(file), Some(text)) => {
                    file.set_text(self).to(text);
                    if file.source_set(self) != change.source_set {
                        membership_changed.extend(file.source_set(self));
                        membership_changed.extend(change.source_set);
                        file.set_source_set(self).to(change.source_set);
                    }
                }
                (None, Some(text)) => {
                    let file = SourceFile::new(
                        self,
                        change.file,
                        text,
                        change.language,
                        change.source_set,
                    );
                    Arc::make_mut(&mut self.files).insert(change.file, file);
                    membership_changed.extend(change.source_set);
                }
                (Some(file), None) => {
                    membership_changed.extend(file.source_set(self));
                    Arc::make_mut(&mut self.files).remove(&change.file);
                }
                (None, None) => {}
            }
        }
        membership_changed.sort();
        membership_changed.dedup();
        let Some(workspace) = self.workspace else {
            return;
        };
        let source_sets = workspace.source_sets(self).clone();
        // One pass over the files groups them by source set.
        let mut grouped: FxHashMap<SourceSetId, Vec<SourceFile>> = membership_changed
            .iter()
            .map(|id| (*id, Vec::new()))
            .collect();
        for file in self.files.values() {
            let group = file.source_set(self).and_then(|id| grouped.get_mut(&id));
            if let Some(group) = group {
                group.push(*file);
            }
        }
        for (id, mut files) in grouped {
            let Some(source_set) = source_sets.get(id.0 as usize).copied() else {
                continue;
            };
            files.sort_by_key(|file| file.file_id(self));
            source_set.set_files(self).to(files.into());
        }
    }

    /// Every file the database knows.
    pub fn all_files(&self) -> Vec<SourceFile> {
        let mut files: Vec<SourceFile> = self.files.values().copied().collect();
        files.sort_by_key(|file| file.file_id(self));
        files
    }
}
