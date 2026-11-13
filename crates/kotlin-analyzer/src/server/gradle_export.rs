//! The Gradle model, exported by the server itself: when the workspace is
//! a Gradle build and no `project.json` is given, the bundled init script
//! runs in the background and writes the model to the cache — nothing is
//! written to the project. The export is redone when a build file is newer
//! than it.

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

/// Whether the model at `model` is missing or older than a build file.
pub(super) fn is_stale(root: &Path, model: &Path) -> bool {
    let Ok(exported) = model.metadata().and_then(|metadata| metadata.modified()) else {
        return true;
    };
    newest_build_file(root).is_some_and(|newest| newest > exported)
}

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
pub(super) fn export(root: &Path, output: &Path) -> Result<(), String> {
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
    fn a_model_older_than_a_build_file_is_stale() {
        let root =
            std::env::temp_dir().join(format!("kotlin-analyzer-export-{}", std::process::id()));
        std::fs::create_dir_all(root.join("app")).unwrap();
        let model = root.join("project.json");
        assert!(is_stale(&root, &model), "a missing model is stale");
        std::fs::write(&model, "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(root.join("app/build.gradle.kts"), "").unwrap();
        assert!(is_stale(&root, &model));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&model, "{}").unwrap();
        assert!(!is_stale(&root, &model));
        assert!(is_gradle_build(&root) == root.join("build.gradle.kts").is_file());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
