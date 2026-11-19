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
    pub libraries: Vec<(hir::LibrarySource, hir::IndexedLibrary)>,
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

/// Several workspace folders (each with its model file, when it has one)
/// read as one workspace.
pub fn read_workspaces(
    folders: &[(PathBuf, Option<PathBuf>)],
) -> Result<LoadedFiles, project_model::LoadError> {
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
    let libraries = index_libraries(&model);
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
/// parallel.
pub fn index_libraries(model: &ProjectModel) -> LoadedLibraries {
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
    let indexed: Vec<Result<hir::IndexedLibrary, String>> =
        sources.par_iter().map(hir::index_library).collect();
    let mut libraries = Vec::with_capacity(sources.len());
    let mut failures = Vec::new();
    for (source, result) in sources.into_iter().zip(indexed) {
        match result {
            Ok(indexed) => libraries.push((source, indexed)),
            Err(error) => {
                failures.push((source.archive.clone(), error));
                // Keep the position, so indices stay valid.
                libraries.push((source, hir::IndexedLibrary::default()));
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
/// (`lib/src.zip`): from the model, or `JAVA_HOME`.
fn jdk_library(model: &ProjectModel) -> Option<hir::LibrarySource> {
    let home = model
        .jdk
        .as_ref()
        .map(|jdk| jdk.home.clone())
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))?;
    let symbols = home.join("lib/ct.sym");
    if !symbols.is_file() {
        return None;
    }
    let declared = model
        .jdk
        .as_ref()
        .and_then(|jdk| jdk.release)
        .or_else(|| agreed_jdk_release(model));
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
    host.set_libraries(libraries.libraries, libraries.by_source_set, libraries.jdk);
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
}
