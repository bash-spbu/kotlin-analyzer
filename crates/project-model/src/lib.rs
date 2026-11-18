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

use rustc_hash::FxHashMap;

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
    /// The kind a source set's name implies. Tests are named as tests:
    /// `test…` (`testDebug`), `androidTest…`, `…Test` (`integrationTest`,
    /// Kotlin Multiplatform's `commonTest`, Android's `androidUnitTest`).
    /// Everything else builds the product: `main`, Kotlin Multiplatform's
    /// `commonMain`, `jvmMain`, `iosMain`…, Android's build types and
    /// flavours (`debug`, `demo`).
    pub fn of_name(name: &str) -> SourceSetKind {
        if name == "testFixtures" {
            return SourceSetKind::TestFixtures;
        }
        let names_tests = name.starts_with("test")
            || name.starts_with("androidTest")
            || name.ends_with("Test")
            || name.ends_with("Tests");
        if names_tests {
            SourceSetKind::Test
        } else {
            SourceSetKind::Main
        }
    }
}

/// What a source set is compiled for, as far as its classpath goes: only
/// JVM (and Android) code sees the JDK.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Platform {
    /// The JVM or Android — and, approximately, Kotlin Multiplatform's
    /// common code, which the build also compiles for them.
    Jvm,
    /// Kotlin/JS, Wasm or Native only.
    NotJvm,
}

/// The source set name prefixes Kotlin Multiplatform uses for targets that
/// aren't the JVM (`jsMain`, `iosArm64Test`, `nativeMain`, `webMain`).
const NOT_JVM_PREFIXES: &[&str] = &[
    "js",
    "wasm",
    "web",
    "native",
    "apple",
    "ios",
    "macos",
    "tvos",
    "watchos",
    "linux",
    "mingw",
    "androidNative",
    "posix",
    "nix",
    "darwin",
    "windows",
];

impl Platform {
    /// The platform a source set's name implies, for a model without one.
    pub fn of_name(name: &str) -> Platform {
        let not_jvm = NOT_JVM_PREFIXES.iter().any(|prefix| {
            name.strip_prefix(prefix).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(|char: char| char.is_ascii_uppercase())
            })
        });
        if not_jvm {
            Platform::NotJvm
        } else {
            Platform::Jvm
        }
    }
}

/// Whether the build resolved a source set's whole classpath. A partial one
/// (a library that couldn't be fetched, a toolchain that isn't installed)
/// leaves names that come from it unknown, so nothing may be called missing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Classpath {
    Complete,
    Partial,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SourceSet {
    pub id: SourceSetId,
    pub module: ModuleId,
    pub name: String,
    pub kind: SourceSetKind,
    pub platform: Platform,
    /// Absolute source directories, generated ones included.
    pub directories: Vec<PathBuf>,
    /// The source sets this one sees, besides itself (transitively closed
    /// when exported by Gradle).
    pub dependencies: Vec<SourceSetId>,
    pub libraries: Vec<Library>,
    pub classpath: Classpath,
    /// What its Kotlin compilation tells the compiler; the defaults when
    /// the build didn't say.
    pub compiler: CompilerOptions,
}

/// What the Kotlin compiler is told that changes what it reports.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct CompilerOptions {
    /// `allWarningsAsErrors`, `-Werror`: a warning fails the build.
    pub warnings_as_errors: bool,
    /// `suppressWarnings`, `-nowarn`: no warning is reported.
    pub suppress_warnings: bool,
    /// Warnings by the compiler's name: `-Xsuppress-warning=NAME`,
    /// `-Xwarning-level=NAME:error`.
    pub warning_levels: Vec<(String, WarningLevel)>,
    /// `optIn`, `-opt-in=…`: the requirement markers opted into.
    pub opt_in: Vec<String>,
    /// The Kotlin API version (`2.2`) the code compiles against.
    pub api_version: Option<String>,
    /// The JDK release the code compiles against: the toolchain's or
    /// `-Xjdk-release`'s.
    pub jdk_release: Option<u32>,
}

impl CompilerOptions {
    /// What the options make the compiler warning `name`: kept as a
    /// warning, an error, or not reported.
    pub fn warning_level(&self, name: &str) -> WarningLevel {
        let own = self
            .warning_levels
            .iter()
            .find(|(named, _)| named == name)
            .map(|(_, level)| *level);
        match own {
            Some(level) => level,
            None if self.suppress_warnings => WarningLevel::Disabled,
            None if self.warnings_as_errors => WarningLevel::Error,
            None => WarningLevel::Warning,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum WarningLevel {
    Warning,
    Error,
    Disabled,
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
    /// Source directory → its source set, for finding a file's source set
    /// by walking up its ancestors.
    directory_index: FxHashMap<PathBuf, SourceSetId>,
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

    pub fn new(
        root: PathBuf,
        origin: ModelOrigin,
        modules: Vec<Module>,
        source_sets: Vec<SourceSet>,
        jdk: Option<Jdk>,
    ) -> ProjectModel {
        let mut directory_index = FxHashMap::default();
        for source_set in &source_sets {
            for directory in &source_set.directories {
                // The first source set to list a directory keeps it.
                directory_index
                    .entry(directory.clone())
                    .or_insert(source_set.id);
            }
        }
        ProjectModel {
            root,
            origin,
            modules,
            source_sets,
            jdk,
            directory_index,
        }
    }

    /// A model of no modules: a workspace not loaded yet.
    pub fn empty(root: PathBuf) -> ProjectModel {
        ProjectModel::new(
            root,
            ModelOrigin::DirectoryConvention,
            Vec::new(),
            Vec::new(),
            None,
        )
    }

    /// The source set whose directory contains `path` most closely.
    pub fn source_set_of(&self, path: &Path) -> Option<SourceSetId> {
        path.ancestors()
            .skip(1)
            .find_map(|ancestor| self.directory_index.get(ancestor).copied())
    }

    /// The package a file's directory implies: its directory relative to
    /// the source root it's under (`src/main/kotlin/a/b/X.kt` → `a.b`);
    /// `None` outside every source root.
    pub fn directory_package(&self, path: &Path) -> Option<intern::Path> {
        let root = path
            .ancestors()
            .skip(1)
            .find(|ancestor| self.directory_index.contains_key(*ancestor))?;
        let directory = path.parent()?.strip_prefix(root).ok()?;
        let segments: Option<Vec<intern::Name>> = directory
            .components()
            .map(|component| component.as_os_str().to_str().map(intern::Name::new))
            .collect();
        Some(intern::Path::new(segments?))
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

#[cfg(test)]
mod platform_tests {
    use super::Platform;

    #[test]
    fn a_source_set_name_tells_jvm_code_from_js_wasm_and_native_code() {
        for jvm in [
            "main",
            "test",
            "commonMain",
            "jvmMain",
            "jvmAndPosixMain",
            "androidMain",
            "debug",
            "jsonMain",
        ] {
            assert_eq!(Platform::of_name(jvm), Platform::Jvm, "{jvm}");
        }
        for other in [
            "jsMain",
            "wasmJsTest",
            "iosArm64Main",
            "nativeMain",
            "webMain",
            "posixMain",
            "linuxX64Test",
        ] {
            assert_eq!(Platform::of_name(other), Platform::NotJvm, "{other}");
        }
    }
}
