//! The incremental database's inputs — file texts, the project model, which
//! files make up each source set — and the one query everything else starts
//! from: parsing a file. Every derived fact is a query over these, so an
//! edit invalidates exactly what depends on it.

pub mod fixture;
mod library;
mod parse_cache;

use std::sync::Arc;

use rustc_hash::FxHashMap;

pub use library::{
    ArchiveKind, ArchiveStamp, Classpaths, IndexedLibrary, IndexedPackage, JdkRelease, Library,
    LibraryPackage, LibrarySource,
};
pub use parse_cache::ParseCache;
pub use project_model::{
    Classpath, CompilerOptions, KotlinVersion, Platform, ProjectModel, SourceSetId, WarningLevel,
};
pub use salsa;
pub use syntax::{Language, Parse};
pub use vfs::FileId;

/// A library's index for a load: read from its archive now, or kept — the
/// one the database has, its archive unchanged since it was read (see
/// [`RootDatabase::library_stamps`]).
#[derive(Clone, Debug)]
pub enum LibraryIndex {
    Read(IndexedLibrary),
    Kept,
}

impl From<IndexedLibrary> for LibraryIndex {
    fn from(indexed: IndexedLibrary) -> LibraryIndex {
        LibraryIndex::Read(indexed)
    }
}

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
    /// The package its directory implies, under its source root.
    #[returns(ref)]
    pub directory_package: Option<intern::Path>,
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
    fn classpaths(&self) -> Classpaths;
    fn parse_cache(&self) -> &ParseCache;
}

/// The syntax tree of a file.
///
/// Not a query: a tree is many times the size of its text, and Salsa keeps
/// a query's results until the next revision however many there are, so
/// one pass over the workspace would hold every file's tree. The bounded
/// [`ParseCache`] keeps the recently used ones instead; reading the text
/// here records the dependency the caller's result has on it.
pub fn parse(db: &dyn SourceDatabase, file: SourceFile) -> Arc<Parse> {
    let text = file.text(db);
    let language = file.language(db);
    db.parse_cache().get_or_parse(text, language)
}

/// The database: Salsa's storage plus the handles of its inputs. Cloning it
/// makes a snapshot that reads while the original waits to write.
#[salsa::db]
#[derive(Clone)]
pub struct RootDatabase {
    storage: salsa::Storage<Self>,
    /// Set when the database is created — empty until the project model is
    /// loaded: what's asked before then sees no source sets.
    workspace: Option<Workspace>,
    files: Arc<FxHashMap<FileId, SourceFile>>,
    classpaths: Option<Classpaths>,
    /// Every library input made so far, by source: a reload re-uses an
    /// unchanged archive's inputs — nothing read from it is read again,
    /// and inputs (which Salsa never frees) don't pile up.
    libraries: Arc<FxHashMap<LibrarySource, KnownLibrary>>,
    /// Shared by snapshots.
    parse_cache: Arc<ParseCache>,
}

/// A library's input and a hash of what it was set from.
#[derive(Clone, Copy)]
struct KnownLibrary {
    input: Library,
    content: u64,
}

impl Default for RootDatabase {
    fn default() -> Self {
        let mut database = Self {
            storage: salsa::Storage::new(None),
            workspace: None,
            files: Arc::default(),
            classpaths: None,
            libraries: Arc::default(),
            parse_cache: Arc::default(),
        };
        database.workspace = Some(Workspace::new(
            &database,
            Arc::new(ProjectModel::empty(std::path::PathBuf::new())),
            Arc::new([]),
        ));
        database.classpaths = Some(Classpaths::new(&database, Arc::new([]), None, None));
        database
    }
}

#[salsa::db]
impl salsa::Database for RootDatabase {}

#[salsa::db]
impl SourceDatabase for RootDatabase {
    fn workspace(&self) -> Workspace {
        self.workspace
            .expect("a workspace is set when the database is created")
    }

    fn source_file(&self, file: FileId) -> Option<SourceFile> {
        self.files.get(&file).copied()
    }

    fn classpaths(&self) -> Classpaths {
        self.classpaths
            .expect("classpaths are set when the database is created")
    }

    fn parse_cache(&self) -> &ParseCache {
        &self.parse_cache
    }
}

/// A file's new state for [`RootDatabase::apply_change`].
pub struct FileChange {
    pub file: FileId,
    /// `None`: deleted.
    pub text: Option<Arc<str>>,
    pub language: Language,
    pub source_set: Option<SourceSetId>,
    pub directory_package: Option<intern::Path>,
}

impl RootDatabase {
    /// Sets the project model, keeping existing files' inputs.
    pub fn set_project_model(&mut self, model: Arc<ProjectModel>) {
        let source_sets = self.source_set_inputs(&model);
        match self.workspace {
            Some(workspace) => {
                use salsa::Setter;
                if *workspace.model(self) != model {
                    workspace.set_model(self).to(model);
                }
                if *workspace.source_sets(self) != source_sets {
                    workspace.set_source_sets(self).to(source_sets);
                }
            }
            None => self.workspace = Some(Workspace::new(self, model, source_sets)),
        }
    }

    /// One input per source set of `model`, holding the files already
    /// known to belong to it. A source set the workspace already had keeps
    /// its input (its file list set only if it changed), so a reload with
    /// the same model recomputes nothing per source set.
    fn source_set_inputs(&mut self, model: &ProjectModel) -> Arc<[SourceSet]> {
        use salsa::Setter;
        let mut grouped: Vec<Vec<SourceFile>> = vec![Vec::new(); model.source_sets.len()];
        for file in self.files.values() {
            let group = file
                .source_set(self)
                .and_then(|id| grouped.get_mut(id.0 as usize));
            if let Some(group) = group {
                group.push(*file);
            }
        }
        let known: Arc<[SourceSet]> = match self.workspace {
            Some(workspace) => workspace.source_sets(self).clone(),
            None => Arc::new([]),
        };
        let mut inputs = Vec::with_capacity(grouped.len());
        for (index, mut files) in grouped.into_iter().enumerate() {
            files.sort_by_key(|file| file.file_id(self));
            let files: Arc<[SourceFile]> = files.into();
            let input = match known.get(index).copied() {
                Some(input) => {
                    if *input.files(self) != files {
                        input.set_files(self).to(files);
                    }
                    input
                }
                None => SourceSet::new(self, SourceSetId(index as u32), files),
            };
            inputs.push(input);
        }
        inputs.into()
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
                    // The same text again (a reload) invalidates nothing.
                    if *file.text(self) != text {
                        file.set_text(self).to(text);
                    }
                    if *file.directory_package(self) != change.directory_package {
                        file.set_directory_package(self)
                            .to(change.directory_package);
                    }
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
                        change.directory_package,
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

    /// Each archive indexed, with its stamp as it was when read: one with
    /// the same stamp now needn't be read again ([`LibraryIndex::Kept`]).
    pub fn library_stamps(&self) -> FxHashMap<LibrarySource, ArchiveStamp> {
        self.libraries
            .iter()
            .map(|(source, known)| (source.clone(), known.input.stamp(self)))
            .collect()
    }

    /// Sets the libraries: each archive's index, which of them each source
    /// set sees (indices into `libraries`), and which one is the JDK.
    pub fn set_libraries<Index: Into<LibraryIndex>>(
        &mut self,
        libraries: Vec<(LibrarySource, Index)>,
        by_source_set: Vec<Vec<usize>>,
        jdk: Option<usize>,
    ) {
        use salsa::Setter;
        let mut inputs: Vec<Library> = Vec::with_capacity(libraries.len());
        for (source, index) in libraries {
            let index: LibraryIndex = index.into();
            let known = self.libraries.get(&source).map(|known| known.input);
            let input = match (index, known) {
                (LibraryIndex::Kept, Some(known)) => known,
                (LibraryIndex::Read(indexed), _) => self.library_input(source, indexed),
                // Kept by a load that started from other stamps than these:
                // nothing to keep — empty until the next load reads it.
                (LibraryIndex::Kept, None) => self.library_input(source, IndexedLibrary::default()),
            };
            inputs.push(input);
        }
        let by_source_set: Arc<[Arc<[Library]>]> = by_source_set
            .into_iter()
            .map(|indices| {
                indices
                    .into_iter()
                    .filter_map(|index| inputs.get(index).copied())
                    .collect()
            })
            .collect();
        let jdk = jdk.and_then(|index| inputs.get(index).copied());
        let classpaths = self.classpaths();
        classpaths.set_by_source_set(self).to(by_source_set);
        classpaths.set_jdk(self).to(jdk);
    }

    /// Keeps libraries' package declarations, once read, under `directory`
    /// (`None`: nowhere).
    pub fn set_library_tree_store(&mut self, directory: Option<std::path::PathBuf>) {
        use salsa::Setter;
        let directory: Option<Arc<std::path::Path>> = directory.map(Arc::from);
        let classpaths = self.classpaths();
        if *classpaths.tree_store(self) != directory {
            classpaths.set_tree_store(self).to(directory);
        }
    }

    /// The input for `source` indexed as `indexed`: the known one when
    /// nothing changed, the known one updated when the archive did, else a
    /// new one.
    fn library_input(&mut self, source: LibrarySource, indexed: IndexedLibrary) -> Library {
        use salsa::Setter;
        let content = content_hash(&indexed);
        let known = self.libraries.get(&source).copied();
        let library = match known {
            Some(known) if known.content == content => return known.input,
            Some(known) => known.input,
            None => Library::new(
                self,
                Arc::new(source.clone()),
                Arc::default(),
                indexed.stamp,
            ),
        };
        if library.stamp(self) != indexed.stamp {
            library.set_stamp(self).to(indexed.stamp);
        }
        let packages = self.package_inputs(library, indexed.packages);
        if **library.packages(self) != packages {
            library.set_packages(self).to(Arc::new(packages));
        }
        Arc::make_mut(&mut self.libraries).insert(
            source,
            KnownLibrary {
                input: library,
                content,
            },
        );
        library
    }

    /// The package inputs of `library` for `packages`: a package it already
    /// had keeps its input, updated where it changed.
    fn package_inputs(
        &mut self,
        library: Library,
        packages: Vec<IndexedPackage>,
    ) -> FxHashMap<intern::Path, LibraryPackage> {
        use salsa::Setter;
        let known = library.packages(self).clone();
        let mut inputs = FxHashMap::default();
        for package in packages {
            // Sorted here, for `declares`' binary search: a `Name` orders by
            // when this process interned it, so an index read back from the
            // disk — its names interned anew, while other threads intern
            // theirs — is in no order of this process's.
            let mut names = package.names;
            names.sort_unstable();
            names.dedup();
            let names: Arc<[intern::Name]> = names.into();
            let entries: Arc<[u32]> = package.entries.into();
            let input = match known.get(&package.package).copied() {
                Some(input) => {
                    if *input.names(self) != names {
                        input.set_names(self).to(names);
                    }
                    if *input.entries(self) != entries {
                        input.set_entries(self).to(entries);
                    }
                    if input.module(self) != package.module {
                        input.set_module(self).to(package.module);
                    }
                    input
                }
                None => LibraryPackage::new(
                    self,
                    library,
                    package.package.clone(),
                    names,
                    entries,
                    package.module,
                ),
            };
            inputs.insert(package.package, input);
        }
        inputs
    }

    /// Every file the database knows.
    pub fn all_files(&self) -> Vec<SourceFile> {
        let mut files: Vec<SourceFile> = self.files.values().copied().collect();
        files.sort_by_key(|file| file.file_id(self));
        files
    }
}

fn content_hash(indexed: &IndexedLibrary) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    indexed.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use intern::{Name, Path};

    use super::*;

    #[test]
    fn a_package_declares_its_names_whatever_order_they_came_in() {
        // Interned in this order, so their `Name`s ascend; given descending,
        // as an index read back in another process may give them.
        let interned: Vec<Name> = ["declares-order-a", "declares-order-b", "declares-order-c"]
            .iter()
            .map(|name| Name::new(name))
            .collect();
        let mut db = RootDatabase::default();
        let source = LibrarySource {
            archive: PathBuf::from("/m2/order.jar"),
            kind: ArchiveKind::Jar,
            sources: None,
        };
        let library = IndexedLibrary {
            packages: vec![IndexedPackage {
                package: Path::parse("a"),
                names: interned.iter().rev().copied().collect(),
                entries: vec![0],
                module: None,
            }],
            stamp: ArchiveStamp {
                modified: None,
                length: 1,
            },
        };
        db.set_libraries(vec![(source, library)], vec![], None);
        let input = db.libraries.values().next().unwrap().input;
        let package = input.packages(&db)[&Path::parse("a")];
        for name in &interned {
            assert!(package.declares(&db, *name), "{name}");
        }
        assert!(!package.declares(&db, Name::new("declares-order-other")));
    }

    fn indexed(names: &[&str], length: u64) -> IndexedLibrary {
        IndexedLibrary {
            packages: vec![IndexedPackage {
                package: Path::parse("a"),
                names: names.iter().map(|name| Name::new(name)).collect(),
                entries: vec![0],
                module: None,
            }],
            stamp: ArchiveStamp {
                modified: None,
                length,
            },
        }
    }

    #[test]
    fn a_reload_reuses_an_unchanged_librarys_inputs_and_updates_a_changed_one() {
        let mut db = RootDatabase::default();
        let source = LibrarySource {
            archive: PathBuf::from("/nowhere/a.jar"),
            kind: ArchiveKind::Jar,
            sources: None,
        };
        let library_of = |db: &RootDatabase| db.classpaths().by_source_set(db)[0][0];
        db.set_libraries(
            vec![(source.clone(), indexed(&["A"], 1))],
            vec![vec![0]],
            None,
        );
        let first = library_of(&db);
        let package = first.packages(&db)[&Path::parse("a")];

        db.set_libraries(
            vec![(source.clone(), indexed(&["A"], 1))],
            vec![vec![0]],
            None,
        );
        assert_eq!(library_of(&db), first, "unchanged: the same input");

        // Rebuilt in place with another class: same inputs, new contents.
        db.set_libraries(vec![(source, indexed(&["A", "B"], 2))], vec![vec![0]], None);
        let library = library_of(&db);
        assert_eq!(library, first);
        assert_eq!(library.packages(&db)[&Path::parse("a")], package);
        assert_eq!(package.names(&db).len(), 2);
        assert_eq!(library.stamp(&db).length, 2);
    }

    #[test]
    fn a_kept_index_is_the_known_librarys_and_its_stamp_is_told() {
        let mut db = RootDatabase::default();
        let source = LibrarySource {
            archive: PathBuf::from("/nowhere/a.jar"),
            kind: ArchiveKind::Jar,
            sources: None,
        };
        let library_of = |db: &RootDatabase| db.classpaths().by_source_set(db)[0][0];
        db.set_libraries(
            vec![(source.clone(), indexed(&["A"], 7))],
            vec![vec![0]],
            None,
        );
        let first = library_of(&db);
        assert_eq!(db.library_stamps()[&source].length, 7);

        db.set_libraries(
            vec![(source.clone(), LibraryIndex::Kept)],
            vec![vec![0]],
            None,
        );
        assert_eq!(library_of(&db), first, "kept: the same input");
        assert_eq!(first.packages(&db)[&Path::parse("a")].names(&db).len(), 1);

        // Kept but never read (stamps of another database): empty, not wrong.
        let other = LibrarySource {
            archive: PathBuf::from("/nowhere/b.jar"),
            ..source
        };
        db.set_libraries(vec![(other, LibraryIndex::Kept)], vec![vec![0]], None);
        assert!(library_of(&db).packages(&db).is_empty());
    }
}
