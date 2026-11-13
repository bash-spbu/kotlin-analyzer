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
    Failed(String),
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
/// build files than those whose newest change is `newest`.
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

/// A build files' state as text: the newest one's modification time.
fn stamp(newest: Option<SystemTime>) -> String {
    match newest {
        Some(time) => {
            let since_epoch = time
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            since_epoch.as_nanos().to_string()
        }
        None => "none".to_owned(),
    }
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

/// Runs the export (the build's wrapper, else `gradle` on the PATH) and
/// writes the model to `output`.
fn export(root: &Path, output: &Path) -> Result<(), String> {
    let directory = output.parent().ok_or("no cache directory")?;
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let script = directory.join("kotlin-analyzer.init.gradle");
    std::fs::write(&script, INIT_SCRIPT).map_err(|error| error.to_string())?;
    let wrapper = root.join("gradlew");
    let program = if wrapper.is_file() {
        wrapper
    } else {
        PathBuf::from("gradle")
    };
    let result = Command::new(&program)
        .current_dir(root)
        .arg("--quiet")
        .arg("--init-script")
        .arg(&script)
        .arg("kotlinAnalyzerModel")
        .arg(format!("-PkotlinAnalyzerOutput={}", output.display()))
        .output()
        .map_err(|error| format!("cannot run {}: {error}", program.display()))?;
    if result.status.success() && output.is_file() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&result.stderr);
    let last_lines: Vec<&str> = stderr.lines().rev().take(12).collect();
    Err(last_lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
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
        assert!(!is_gradle_build(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
