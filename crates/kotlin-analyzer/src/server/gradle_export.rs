//! The Gradle model, exported by the server itself: when the workspace is
//! a Gradle build and no `project.json` is given, the bundled init script
//! runs in the background and writes the model to the cache — nothing is
//! written to the project. The export is redone when the build files
//! changed since it was made.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use walkdir::WalkDir;

use super::library_sources::cache_directory;

/// The init script (`editors/gradle/kotlin-analyzer.init.gradle`).
const INIT_SCRIPT: &str = include_str!("../../../../editors/gradle/kotlin-analyzer.init.gradle");

/// Files whose change changes the model.
const BUILD_FILES: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "libs.versions.toml",
];

/// Where the model of the build at `root` is cached.
pub(super) fn cached_model(root: &Path) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    Some(
        cache_directory()?
            .join("projects")
            .join(format!("{name}-{:016x}", hasher.finish()))
            .join("project.json"),
    )
}

/// Whether a change to `path` can change the model.
pub(super) fn is_build_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| BUILD_FILES.contains(&name))
}

/// Glob patterns of the build files, for a file watcher.
pub(super) fn build_file_globs() -> impl Iterator<Item = String> {
    BUILD_FILES.iter().map(|name| format!("**/{name}"))
}

pub(super) fn is_gradle_build(root: &Path) -> bool {
    [
        "settings.gradle",
        "settings.gradle.kts",
        "build.gradle",
        "build.gradle.kts",
    ]
    .iter()
    .any(|name| root.join(name).is_file())
}

/// What a background export did.
pub(super) enum Export {
    /// The cached model was exported after the build files' last change.
    UpToDate,
    Exported,
    Failed(ExportFailure),
}

/// Why an export failed: for the log, its kind (and Gradle's exit code);
/// for the user, what Gradle said.
#[derive(Debug)]
pub(super) struct ExportFailure {
    pub(super) kind: ExportFailureKind,
    pub(super) exit_code: Option<i32>,
    /// Shown to the user, never logged: it may name their paths and code.
    pub(super) detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ExportFailureKind {
    /// The cache directory couldn't be written.
    Cache,
    /// Neither the wrapper nor `gradle` could be started.
    CannotRun,
    /// Gradle ran and failed.
    BuildFailed,
    /// Gradle succeeded without writing the model.
    NoModel,
}

impl ExportFailureKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            ExportFailureKind::Cache => "cache-unwritable",
            ExportFailureKind::CannotRun => "cannot-run-gradle",
            ExportFailureKind::BuildFailed => "build-failed",
            ExportFailureKind::NoModel => "no-model-written",
        }
    }
}

/// Exports the model of the build at `root` to `output` unless the cached
/// one is current. Current means: exported when the newest build file had
/// the modification time it has now (recorded next to the model) — not
/// "newer than the build files", which a build file stamped in the future
/// would never let be true.
pub(super) fn export_if_stale(root: &Path, output: &Path) -> Export {
    let newest = newest_build_file(root);
    if !is_stale(output, newest) {
        return Export::UpToDate;
    }
    match export(root, output) {
        Ok(()) => {
            // Without the stamp the next check exports again: harmless.
            let _ = std::fs::write(stamp_path(output), stamp(newest));
            Export::Exported
        }
        Err(error) => Export::Failed(error),
    }
}

/// Whether the model at `model` is missing, or was exported for other
/// build files than those whose newest change is `newest`, or by another
/// init script (one that exports less: compiler options came later).
fn is_stale(model: &Path, newest: Option<SystemTime>) -> bool {
    if !model.is_file() {
        return true;
    }
    let recorded = std::fs::read_to_string(stamp_path(model)).ok();
    recorded.as_deref() != Some(stamp(newest).as_str())
}

fn stamp_path(model: &Path) -> PathBuf {
    model.with_extension("stamp")
}

/// What a model was exported for, as text: this init script, and the
/// build files' newest modification time.
fn stamp(newest: Option<SystemTime>) -> String {
    stamp_for(INIT_SCRIPT, newest)
}

fn stamp_for(script: &str, newest: Option<SystemTime>) -> String {
    let mut hasher = rustc_hash::FxHasher::default();
    std::hash::Hash::hash(script, &mut hasher);
    let script_hash = std::hash::Hasher::finish(&hasher);
    let build_files = match newest {
        Some(time) => {
            let since_epoch = time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            since_epoch.as_nanos().to_string()
        }
        None => "none".to_owned(),
    };
    format!("script {script_hash:016x} build {build_files}")
}

/// The newest modification time among the build files under `root`.
fn newest_build_file(root: &Path) -> Option<SystemTime> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_str().unwrap_or("");
            !project_model::is_excluded_directory(name) || entry.depth() == 0
        })
        .flatten()
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| BUILD_FILES.contains(&name))
        })
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max()
}

/// The export's tasks read projects while they run, which the
/// configuration cache and isolated projects forbid — builds that turn
/// them on in `gradle.properties` get them turned off for the export.
const EXPORT_ARGUMENTS: &[&str] = &[
    "--quiet",
    "--no-configuration-cache",
    "-Dorg.gradle.isolated-projects=false",
    "-Dorg.gradle.unsafe.isolated-projects=false",
];

/// Runs the export (the build's wrapper, else `gradle` on the PATH) and
/// writes the model to `output`.
fn export(root: &Path, output: &Path) -> Result<(), ExportFailure> {
    let failure = |kind, exit_code, detail: String| ExportFailure {
        kind,
        exit_code,
        detail,
    };
    let cache_failure =
        |error: std::io::Error| failure(ExportFailureKind::Cache, None, error.to_string());
    let directory = output.parent().ok_or_else(|| {
        failure(
            ExportFailureKind::Cache,
            None,
            "no cache directory".to_owned(),
        )
    })?;
    std::fs::create_dir_all(directory).map_err(cache_failure)?;
    let script = directory.join("kotlin-analyzer.init.gradle");
    std::fs::write(&script, INIT_SCRIPT).map_err(cache_failure)?;
    let wrapper = root.join("gradlew");
    let program = if wrapper.is_file() {
        wrapper
    } else {
        PathBuf::from("gradle")
    };
    let result = Command::new(&program)
        .current_dir(root)
        .args(EXPORT_ARGUMENTS)
        .arg("--init-script")
        .arg(&script)
        .arg("kotlinAnalyzerModel")
        .arg(format!("-PkotlinAnalyzerOutput={}", output.display()))
        .output()
        .map_err(|error| {
            failure(
                ExportFailureKind::CannotRun,
                None,
                format!("cannot run {}: {error}", program.display()),
            )
        })?;
    if result.status.success() && output.is_file() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&result.stderr);
    let last_lines: Vec<&str> = stderr.lines().rev().take(12).collect();
    let detail = last_lines.into_iter().rev().collect::<Vec<_>>().join("\n");
    let kind = if result.status.success() {
        ExportFailureKind::NoModel
    } else {
        ExportFailureKind::BuildFailed
    };
    Err(failure(kind, result.status.code(), detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_stale_until_exported_for_the_build_files_as_they_are() {
        let root =
            std::env::temp_dir().join(format!("kotlin-analyzer-export-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        let build_file = root.join("app/build.gradle.kts");
        std::fs::write(&build_file, "").unwrap();
        let model = root.join("project.json");
        assert!(
            is_stale(&model, newest_build_file(&root)),
            "a missing model"
        );
        std::fs::write(&model, "{}").unwrap();
        assert!(is_stale(&model, newest_build_file(&root)), "no stamp yet");
        std::fs::write(stamp_path(&model), stamp(newest_build_file(&root))).unwrap();
        assert!(!is_stale(&model, newest_build_file(&root)));

        // A build file stamped in the future: once exported for it, the
        // model stays current — no export after export.
        let future = SystemTime::now() + std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&build_file)
            .unwrap()
            .set_modified(future)
            .unwrap();
        assert!(
            is_stale(&model, newest_build_file(&root)),
            "a build file changed"
        );
        std::fs::write(stamp_path(&model), stamp(newest_build_file(&root))).unwrap();
        assert!(!is_stale(&model, newest_build_file(&root)));

        // Exported by another init script: stale, whatever the build files.
        let older_script = stamp_for("// an older init script", newest_build_file(&root));
        std::fs::write(stamp_path(&model), older_script).unwrap();
        assert!(
            is_stale(&model, newest_build_file(&root)),
            "another init script"
        );
        assert!(!is_gradle_build(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_export_runs_without_the_configuration_cache_and_isolated_projects() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "kotlin-analyzer-export-arguments-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A wrapper that records its arguments and writes the model where
        // it's told to.
        let wrapper = root.join("gradlew");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > arguments\nfor argument in \"$@\"; do case \"$argument\" in -PkotlinAnalyzerOutput=*) echo '{}' > \"${argument#-PkotlinAnalyzerOutput=}\";; esac; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = root.join("cache/project.json");

        export(&root, &output).unwrap();

        let arguments = std::fs::read_to_string(root.join("arguments")).unwrap();
        let arguments: Vec<&str> = arguments.lines().collect();
        for expected in [
            "--no-configuration-cache",
            "-Dorg.gradle.isolated-projects=false",
            "kotlinAnalyzerModel",
        ] {
            assert!(arguments.contains(&expected), "{expected} in {arguments:?}");
        }
        assert!(output.is_file());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
