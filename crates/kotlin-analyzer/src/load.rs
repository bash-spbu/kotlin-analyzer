//! Reading a workspace from disk: its project model and every source file of
//! every source set. Shared by the server and the command-line tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ide::AnalysisHost;
use project_model::ProjectModel;
use rayon::prelude::*;
use rustc_hash::FxHashSet;
use syntax::Language;
use vfs::Vfs;

/// The workspace as read from disk, before it is put into a database.
pub struct LoadedFiles {
    pub model: Arc<ProjectModel>,
    pub files: Vec<(PathBuf, Arc<str>)>,
    pub libraries: LoadedLibraries,
    pub model_time: Duration,
    pub discovery_time: Duration,
    pub read_time: Duration,
    pub library_time: Duration,
}

/// Every library archive the workspace compiles against, indexed.
#[derive(Default)]
pub struct LoadedLibraries {
    /// Each archive's index: read now, or kept from the last load.
    pub libraries: Vec<(hir::LibrarySource, hir::LibraryIndex)>,
    /// Per source set (by id), indices into `libraries`, in classpath order.
    pub by_source_set: Vec<Vec<usize>>,
    pub jdk: Option<usize>,
    /// Archives that couldn't be read.
    pub failures: Vec<(PathBuf, String)>,
}

pub fn read_workspace(
    root: &Path,
    project_json: Option<&Path>,
) -> Result<LoadedFiles, project_model::LoadError> {
    read_workspaces(&[(root.to_path_buf(), project_json.map(Path::to_path_buf))])
}

/// A step of reading a workspace, for a progress bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoadStep {
    ReadingModel,
    ReadingFiles { count: usize },
    IndexingLibraries { indexed: usize, total: usize },
}

/// Several workspace folders (each with its model file, when it has one)
/// read as one workspace.
pub fn read_workspaces(
    folders: &[(PathBuf, Option<PathBuf>)],
) -> Result<LoadedFiles, project_model::LoadError> {
    read_workspaces_reporting(folders, &IndexedStamps::default(), &|_| {})
}

/// [`read_workspaces`], telling `report` each step as it comes.
/// The archives indexed by an earlier load, with their stamps then.
pub type IndexedStamps = rustc_hash::FxHashMap<hir::LibrarySource, hir::ArchiveStamp>;

/// The workspace of `folders`, reporting its steps; an archive in `indexed`
/// with the same stamp now isn't read again.
pub fn read_workspaces_reporting(
    folders: &[(PathBuf, Option<PathBuf>)],
    indexed: &IndexedStamps,
    report: &(dyn Fn(LoadStep) + Sync),
) -> Result<LoadedFiles, project_model::LoadError> {
    report(LoadStep::ReadingModel);
    let started = Instant::now();
    let models = folders
        .iter()
        .map(|(root, project_json)| ProjectModel::load(root, project_json.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    let model = ProjectModel::combined(models);
    let model_time = started.elapsed();

    let started = Instant::now();
    let paths = source_paths(&model);
    let discovery_time = started.elapsed();

    report(LoadStep::ReadingFiles { count: paths.len() });
    let started = Instant::now();
    let files: Vec<(PathBuf, Arc<str>)> = paths
        .into_par_iter()
        .filter_map(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let text: Arc<str> = Arc::from(String::from_utf8_lossy(&bytes).as_ref());
            Some((path, text))
        })
        .collect();
    let read_time = started.elapsed();

    let started = Instant::now();
    // Tests leave the user's cache alone.
    let index_cache = if cfg!(test) {
        None
    } else {
        crate::server::cache_directory().map(|cache| crate::library_index_cache::directory(&cache))
    };
    let libraries = index_libraries(&model, indexed, index_cache.as_deref(), report);
    let library_time = started.elapsed();
    Ok(LoadedFiles {
        model: Arc::new(model),
        files,
        libraries,
        model_time,
        discovery_time,
        read_time,
        library_time,
    })
}

/// Indexes the JDK and every distinct JAR of the model's classpaths, in
/// parallel — but for those `indexed` already, unchanged since, and those
/// whose index `index_cache` (the on-disk one) has.
pub fn index_libraries(
    model: &ProjectModel,
    indexed: &IndexedStamps,
    index_cache: Option<&Path>,
    report: &(dyn Fn(LoadStep) + Sync),
) -> LoadedLibraries {
    let mut sources: Vec<hir::LibrarySource> = Vec::new();
    let mut by_source_set: Vec<Vec<usize>> = Vec::with_capacity(model.source_sets.len());
    let mut positions: rustc_hash::FxHashMap<PathBuf, usize> = rustc_hash::FxHashMap::default();
    for source_set in &model.source_sets {
        let mut indices = Vec::with_capacity(source_set.libraries.len());
        for library in &source_set.libraries {
            let position = *positions.entry(library.jar.clone()).or_insert_with(|| {
                sources.push(hir::LibrarySource {
                    archive: library.jar.clone(),
                    kind: hir::ArchiveKind::Jar,
                    sources: library.sources_jar.clone(),
                });
                sources.len() - 1
            });
            indices.push(position);
        }
        by_source_set.push(indices);
    }
    let jdk = jdk_library(model).map(|jdk| {
        sources.push(jdk);
        sources.len() - 1
    });
    let total = sources.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    if total > 0 {
        report(LoadStep::IndexingLibraries { indexed: 0, total });
    }
    let read: Vec<Result<hir::LibraryIndex, String>> = sources
        .par_iter()
        .map(|source| {
            let stamp = hir::ArchiveStamp::of(&source.archive);
            let unchanged = indexed.get(source).is_some_and(|known| *known == stamp);
            let cached = || {
                index_cache.and_then(|directory| {
                    crate::library_index_cache::read(directory, source, stamp)
                })
            };
            let index = if unchanged {
                Ok(hir::LibraryIndex::Kept)
            } else if let Some(cached) = cached() {
                Ok(hir::LibraryIndex::Read(cached))
            } else {
                let read = hir::index_library(source);
                if let (Ok(read), Some(directory)) = (&read, index_cache) {
                    crate::library_index_cache::write(directory, source, read);
                }
                read.map(hir::LibraryIndex::Read)
            };
            let count = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            report(LoadStep::IndexingLibraries {
                indexed: count,
                total,
            });
            index
        })
        .collect();
    let mut libraries = Vec::with_capacity(sources.len());
    let mut failures = Vec::new();
    for (source, result) in sources.into_iter().zip(read) {
        match result {
            Ok(index) => libraries.push((source, index)),
            Err(error) => {
                failures.push((source.archive.clone(), error));
                // Keep the position, so indices stay valid.
                let empty = hir::LibraryIndex::Read(hir::IndexedLibrary::default());
                libraries.push((source, empty));
            }
        }
    }
    LoadedLibraries {
        libraries,
        by_source_set,
        jdk,
        failures,
    }
}

/// The JDK's API (`lib/ct.sym`, for the project's release) and sources
/// (`lib/src.zip`): from the model, `JAVA_HOME`, or a JDK in `~/.jdks`.
fn jdk_library(model: &ProjectModel) -> Option<hir::LibrarySource> {
    let home = model.jdk.as_ref().map(|jdk| jdk.home.clone()).or_else(|| {
        let runtime = project_model::jdk::JavaEnvironment::current().runtime()?;
        runtime.home().map(Path::to_path_buf)
    })?;
    let symbols = home.join("lib/ct.sym");
    if !symbols.is_file() {
        return None;
    }
    let declared = declared_jdk_release(model);
    let (release, known) = match declared {
        Some(release) => (release, hir::JdkRelease::Declared),
        None => (java_version(&home)?, hir::JdkRelease::RunningJdk),
    };
    let letter = hir::release_letter(release)?;
    let source_archive = home.join("lib/src.zip");
    Some(hir::LibrarySource {
        archive: symbols,
        kind: hir::ArchiveKind::JdkSymbols {
            release: letter,
            known,
        },
        sources: source_archive.is_file().then_some(source_archive),
    })
}

/// The JDK release the project compiles against: the model's, else the one
/// every JVM source set agrees on.
pub(crate) fn declared_jdk_release(model: &ProjectModel) -> Option<u32> {
    model
        .jdk
        .as_ref()
        .and_then(|jdk| jdk.release)
        .or_else(|| agreed_jdk_release(model))
}

/// The JDK release every JVM source set compiles against, when they all
/// name the same one (their toolchain's or `-Xjdk-release`'s).
fn agreed_jdk_release(model: &ProjectModel) -> Option<u32> {
    let mut releases = model
        .source_sets
        .iter()
        .filter(|source_set| source_set.platform == project_model::Platform::Jvm)
        .map(|source_set| source_set.compiler.jdk_release);
    let first = releases.next()??;
    releases
        .all(|release| release == Some(first))
        .then_some(first)
}

/// The major version in a JDK's `release` file: `JAVA_VERSION="25.0.5"`.
fn java_version(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let line = text
        .lines()
        .find(|line| line.starts_with("JAVA_VERSION="))?;
    let version = line.trim_start_matches("JAVA_VERSION=").trim_matches('"');
    let major = version.split('.').next()?;
    // Java 8 reports `1.8.0_…`.
    if major == "1" {
        return version.split('.').nth(1)?.parse().ok();
    }
    major.parse().ok()
}

/// Every Kotlin and Java file under the model's source directories.
pub fn source_paths(model: &ProjectModel) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = model
        .all_source_directories()
        .flat_map(|(_, directory)| {
            walkdir::WalkDir::new(directory)
                .into_iter()
                .filter_entry(|entry| {
                    let nested_excluded = entry.depth() > 0
                        && entry.file_type().is_dir()
                        && project_model::is_excluded_directory(
                            &entry.file_name().to_string_lossy(),
                        );
                    !nested_excluded
                })
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .filter(|entry| Language::of_path(entry.path()).is_some())
                .map(|entry| entry.into_path())
        })
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Puts a (re)loaded workspace into `host`: each file's text (unless
/// `keep` says an open editor buffer owns it), files no longer in the
/// workspace removed, and every file's source set and package directory
/// read from `model` again — a reload changes those where no text changed.
pub fn apply_files(
    host: &mut AnalysisHost,
    vfs: &mut Vfs,
    model: &Arc<ProjectModel>,
    files: Vec<(PathBuf, Arc<str>)>,
    keep: impl Fn(&Path) -> bool,
) {
    let loaded: FxHashSet<PathBuf> = files.iter().map(|(path, _)| path.clone()).collect();
    for (path, text) in files {
        if keep(&path) {
            continue;
        }
        vfs.set_contents(&path, Some(text));
    }
    let gone: Vec<PathBuf> = vfs
        .existing_files()
        .map(|file| vfs.path(file).to_path_buf())
        .filter(|path| !loaded.contains(path) && !keep(path))
        .collect();
    for path in gone {
        vfs.set_contents(&path, None);
    }
    let mut changes = file_changes(vfs, model);
    let changed: FxHashSet<vfs::FileId> = changes.iter().map(|change| change.file).collect();
    let unchanged: Vec<vfs::FileId> = vfs
        .existing_files()
        .filter(|file| !changed.contains(file))
        .collect();
    changes.extend(
        unchanged
            .into_iter()
            .filter_map(|file| file_change(vfs, model, file, vfs.contents(file).cloned())),
    );
    host.apply_change(changes);
    host.set_project_model(model.clone());
}

/// Hands the indexed libraries to `host`.
pub fn apply_libraries(host: &mut AnalysisHost, libraries: LoadedLibraries) {
    // Tests leave the user's cache alone.
    let tree_store = if cfg!(test) {
        None
    } else {
        crate::server::cache_directory().map(|cache| library_tree_store(&cache))
    };
    host.set_library_tree_store(tree_store);
    host.set_libraries(libraries.libraries, libraries.by_source_set, libraries.jdk);
}

/// Where libraries' package declarations are kept, under the cache
/// directory `cache` (`docs/spec/libraries.md`).
pub fn library_tree_store(cache: &Path) -> PathBuf {
    cache.join("library-trees")
}

/// The changes `vfs` recorded, as database file changes.
pub fn file_changes(vfs: &mut Vfs, model: &ProjectModel) -> Vec<hir::FileChange> {
    vfs.take_changes()
        .into_iter()
        .filter_map(|changed| {
            let text = match changed.change {
                vfs::Change::Written(text) => Some(text),
                vfs::Change::Deleted => None,
            };
            file_change(vfs, model, changed.file, text)
        })
        .collect()
}

/// `file` with `text` (`None`: deleted), placed by `model`.
fn file_change(
    vfs: &Vfs,
    model: &ProjectModel,
    file: vfs::FileId,
    text: Option<Arc<str>>,
) -> Option<hir::FileChange> {
    let path = vfs.path(file);
    Some(hir::FileChange {
        file,
        text,
        language: Language::of_path(path)?,
        source_set: model.source_set_of(path),
        directory_package: model.directory_package(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_with_releases(releases: &[(project_model::Platform, Option<u32>)]) -> ProjectModel {
        let source_sets = releases
            .iter()
            .enumerate()
            .map(|(index, (platform, release))| project_model::SourceSet {
                id: project_model::SourceSetId(index as u32),
                module: project_model::ModuleId(0),
                name: format!("set{index}"),
                kind: project_model::SourceSetKind::Main,
                platform: *platform,
                directories: Vec::new(),
                dependencies: Vec::new(),
                libraries: Vec::new(),
                classpath: project_model::Classpath::Complete,
                compiler: project_model::CompilerOptions {
                    jdk_release: *release,
                    ..project_model::CompilerOptions::default()
                },
            })
            .collect();
        ProjectModel::new(
            PathBuf::from("/work"),
            project_model::ModelOrigin::GradleExport,
            Vec::new(),
            source_sets,
            None,
        )
    }

    #[test]
    fn the_jdk_release_is_declared_only_when_every_jvm_source_set_agrees() {
        use project_model::Platform::{Jvm, NotJvm};
        assert_eq!(
            agreed_jdk_release(&model_with_releases(&[
                (Jvm, Some(8)),
                (Jvm, Some(8)),
                (NotJvm, None)
            ])),
            Some(8)
        );
        assert_eq!(
            agreed_jdk_release(&model_with_releases(&[(Jvm, Some(8)), (Jvm, Some(17))])),
            None
        );
        assert_eq!(
            agreed_jdk_release(&model_with_releases(&[(Jvm, Some(8)), (Jvm, None)])),
            None
        );
        assert_eq!(agreed_jdk_release(&model_with_releases(&[])), None);
    }

    #[test]
    fn a_reload_reads_again_only_the_archives_that_changed() {
        let directory =
            std::env::temp_dir().join(format!("kotlin-analyzer-reindex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let write_jar = |name: &str, entries: &[&str]| -> PathBuf {
            let path = directory.join(name);
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for entry in entries {
                writer
                    .start_file(*entry, zip::write::SimpleFileOptions::default())
                    .unwrap();
                std::io::Write::write_all(&mut writer, b"not a class").unwrap();
            }
            writer.finish().unwrap();
            path
        };
        let kept = write_jar("kept.jar", &["a/A.txt"]);
        let changed = write_jar("changed.jar", &["b/B.txt"]);
        let mut model = model_with_releases(&[(project_model::Platform::Jvm, None)]);
        model.source_sets[0].libraries = [&kept, &changed]
            .into_iter()
            .map(|jar| project_model::Library {
                coordinates: None,
                jar: jar.clone(),
                sources_jar: None,
            })
            .collect();
        model.jdk = None;
        let kinds = |loaded: &LoadedLibraries| -> Vec<(String, &'static str)> {
            loaded
                .libraries
                .iter()
                .filter(|(source, _)| source.kind == hir::ArchiveKind::Jar)
                .map(|(source, index)| {
                    let name = source
                        .archive
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    let kind = match index {
                        hir::LibraryIndex::Read(_) => "read",
                        hir::LibraryIndex::Kept => "kept",
                    };
                    (name, kind)
                })
                .collect()
        };
        let first = index_libraries(&model, &IndexedStamps::default(), None, &|_| {});
        assert_eq!(
            kinds(&first),
            [
                ("kept.jar".to_owned(), "read"),
                ("changed.jar".to_owned(), "read")
            ]
        );
        let mut host = ide::AnalysisHost::default();
        apply_libraries(&mut host, first);
        // One archive rebuilt in place: longer.
        write_jar("changed.jar", &["b/B.txt", "b/C.txt"]);
        let second = index_libraries(&model, &host.library_stamps(), None, &|_| {});
        assert_eq!(
            kinds(&second),
            [
                ("kept.jar".to_owned(), "kept"),
                ("changed.jar".to_owned(), "read")
            ]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn after_a_restart_an_unchanged_archives_index_comes_from_the_disk() {
        let directory =
            std::env::temp_dir().join(format!("kotlin-analyzer-disk-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let jar = directory.join("client.jar");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&jar).unwrap());
        writer
            .start_file(
                "com/example/Client.txt",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        std::io::Write::write_all(&mut writer, b"not a class").unwrap();
        writer.finish().unwrap();
        let mut model = model_with_releases(&[(project_model::Platform::Jvm, None)]);
        model.source_sets[0].libraries = vec![project_model::Library {
            coordinates: None,
            jar: jar.clone(),
            sources_jar: None,
        }];
        let cache = directory.join("library-index");
        let jar_index = |loaded: LoadedLibraries| {
            loaded
                .libraries
                .into_iter()
                .find(|(source, _)| source.archive == jar)
                .map(|(_, index)| index)
                .unwrap()
        };
        let first = jar_index(index_libraries(
            &model,
            &IndexedStamps::default(),
            Some(&cache),
            &|_| {},
        ));
        let hir::LibraryIndex::Read(indexed) = first else {
            panic!("read the first time");
        };
        // The archive's bytes replaced, its length and time kept: read again
        // it would index as nothing; from the disk, it's what it was.
        let modified = std::fs::metadata(&jar).unwrap().modified().unwrap();
        let length = std::fs::metadata(&jar).unwrap().len() as usize;
        std::fs::write(&jar, vec![b'x'; length]).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&jar)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        // A restart: nothing known in memory.
        let again = jar_index(index_libraries(
            &model,
            &IndexedStamps::default(),
            Some(&cache),
            &|_| {},
        ));
        let hir::LibraryIndex::Read(from_disk) = again else {
            panic!("read from the disk");
        };
        assert_eq!(from_disk, indexed);
        // Without the cache the garbage is what's read.
        let uncached = index_libraries(&model, &IndexedStamps::default(), None, &|_| {});
        assert!(
            uncached.failures.iter().any(|(archive, _)| *archive == jar),
            "the replaced archive can't be read"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
