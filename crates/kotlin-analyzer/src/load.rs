//! Reading a workspace from disk: its project model and every source file of
//! every source set. Shared by the server and the command-line tools.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ide::AnalysisHost;
use project_model::ProjectModel;
use rayon::prelude::*;
use syntax::Language;
use vfs::Vfs;

/// The workspace as read from disk, before it is put into a database.
pub struct LoadedFiles {
    pub model: Arc<ProjectModel>,
    pub files: Vec<(PathBuf, Arc<str>)>,
    pub model_time: Duration,
    pub discovery_time: Duration,
    pub read_time: Duration,
}

pub fn read_workspace(root: &Path, project_json: Option<&Path>) -> Result<LoadedFiles, String> {
    let started = Instant::now();
    let model = ProjectModel::load(root, project_json).map_err(|error| error.to_string())?;
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
    Ok(LoadedFiles {
        model: Arc::new(model),
        files,
        model_time,
        discovery_time,
        read_time,
    })
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

/// Records each file's text in `vfs` (unless `keep` says an open editor
/// buffer owns it) and hands the resulting changes to `host`.
pub fn apply_files(
    host: &mut AnalysisHost,
    vfs: &mut Vfs,
    model: &Arc<ProjectModel>,
    files: Vec<(PathBuf, Arc<str>)>,
    keep: impl Fn(&Path) -> bool,
) {
    for (path, text) in files {
        if keep(&path) {
            continue;
        }
        vfs.set_contents(&path, Some(text));
    }
    let changes = file_changes(vfs, model);
    host.apply_change(changes);
    host.set_project_model(model.clone());
}

/// The changes `vfs` recorded, as database file changes.
pub fn file_changes(vfs: &mut Vfs, model: &ProjectModel) -> Vec<hir::FileChange> {
    vfs.take_changes()
        .into_iter()
        .filter_map(|changed| {
            let path = vfs.path(changed.file).to_path_buf();
            let language = Language::of_path(&path)?;
            let text = match changed.change {
                vfs::Change::Written(text) => Some(text),
                vfs::Change::Deleted => None,
            };
            Some(hir::FileChange {
                file: changed.file,
                text,
                language,
                source_set: model.source_set_of(&path),
            })
        })
        .collect()
}
