//! What a workspace consists of: Gradle modules, their source sets (main,
//! test, test fixtures — generated directories included), which source sets
//! each one sees, each one's libraries, and the JDK.
//!
//! Read from `.kotlin-analyzer/project.json`, which the Gradle init script in
//! `editors/gradle` writes; guessed from directory conventions when it is
//! missing. See `docs/spec/project-model.md`.

mod convention;
mod project_json;

use std::path::{Path, PathBuf};

pub use convention::is_excluded_directory;

/// Where the model's facts came from — how far they can be trusted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelOrigin {
    /// Exported by Gradle: module dependencies and library versions are the
    /// build's own.
    GradleExport,
    /// Guessed from directory layout: every module may see every other one,
    /// and libraries are unknown.
    DirectoryConvention,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ModuleId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SourceSetId(pub u32);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Module {
    /// The Gradle project path, `:payments:api` (`:` for the root project).
    pub gradle_path: String,
    pub directory: PathBuf,
    pub source_sets: Vec<SourceSetId>,
}

/// What a source set is for — which decides what may see it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SourceSetKind {
    Main,
    TestFixtures,
    /// `test`, `integrationTest` and any other source set built on main.
    Test,
}

impl SourceSetKind {
    pub fn of_name(name: &str) -> SourceSetKind {
        match name {
            "main" => SourceSetKind::Main,
            "testFixtures" => SourceSetKind::TestFixtures,
            _ => SourceSetKind::Test,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceSet {
    pub id: SourceSetId,
    pub module: ModuleId,
    pub name: String,
    pub kind: SourceSetKind,
    /// Absolute source directories, generated ones included.
    pub directories: Vec<PathBuf>,
    /// The source sets this one sees, besides itself (transitively closed
    /// when exported by Gradle).
    pub dependencies: Vec<SourceSetId>,
    pub libraries: Vec<Library>,
}

/// A classpath entry of a source set.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Library {
    /// `group:artifact:version`, when the build knows it.
    pub coordinates: Option<String>,
    pub jar: PathBuf,
    pub sources_jar: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Jdk {
    pub home: PathBuf,
    pub release: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectModel {
    pub root: PathBuf,
    pub origin: ModelOrigin,
    pub modules: Vec<Module>,
    pub source_sets: Vec<SourceSet>,
    pub jdk: Option<Jdk>,
}

/// Why `project.json` couldn't be used.
#[derive(Debug)]
pub enum LoadError {
    Unreadable(std::io::Error),
    Malformed(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Unreadable(error) => write!(formatter, "cannot read project.json: {error}"),
            LoadError::Malformed(error) => write!(formatter, "malformed project.json: {error}"),
        }
    }
}

/// The exported model's location under a workspace root.
pub const PROJECT_JSON: &str = ".kotlin-analyzer/project.json";

impl ProjectModel {
    /// The Gradle export when there is one (or `project_json` overrides
    /// where it is read from), otherwise directory conventions.
    pub fn load(root: &Path, project_json: Option<&Path>) -> Result<ProjectModel, LoadError> {
        let exported = project_json
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join(PROJECT_JSON));
        if exported.is_file() {
            let text = std::fs::read_to_string(&exported).map_err(LoadError::Unreadable)?;
            return project_json::parse(root, &text);
        }
        Ok(convention::discover(root))
    }

    pub fn source_set(&self, id: SourceSetId) -> &SourceSet {
        &self.source_sets[id.0 as usize]
    }

    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.0 as usize]
    }

    /// The source set whose directory contains `path` most closely.
    pub fn source_set_of(&self, path: &Path) -> Option<SourceSetId> {
        let mut best: Option<(usize, SourceSetId)> = None;
        for source_set in &self.source_sets {
            for directory in &source_set.directories {
                if !path.starts_with(directory) {
                    continue;
                }
                let depth = directory.components().count();
                let deeper = best.is_none_or(|(best_depth, _)| depth > best_depth);
                if deeper {
                    best = Some((depth, source_set.id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// `id` and every source set it sees.
    pub fn visible_source_sets(&self, id: SourceSetId) -> Vec<SourceSetId> {
        let mut visible = vec![id];
        let mut index = 0;
        while index < visible.len() {
            for dependency in &self.source_set(visible[index]).dependencies {
                if !visible.contains(dependency) {
                    visible.push(*dependency);
                }
            }
            index += 1;
        }
        visible
    }

    /// Every source directory of every source set.
    pub fn all_source_directories(&self) -> impl Iterator<Item = (SourceSetId, &Path)> {
        self.source_sets.iter().flat_map(|source_set| {
            source_set
                .directories
                .iter()
                .map(move |directory| (source_set.id, directory.as_path()))
        })
    }
}
