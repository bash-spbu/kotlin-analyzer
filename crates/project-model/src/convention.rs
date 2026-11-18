//! The model guessed from directory layout, when Gradle hasn't exported one:
//! a module per `build.gradle(.kts)`, a source set per `src/<name>/kotlin`
//! or `src/<name>/java`.

use std::path::{Path, PathBuf};

use crate::{
    Classpath, CompilerOptions, ModelOrigin, Module, ModuleId, Platform, ProjectModel, SourceSet,
    SourceSetId, SourceSetKind,
};

/// Directories that never hold workspace sources (unless a model lists one
/// explicitly, as Gradle does for generated code under `build/`).
pub fn is_excluded_directory(name: &str) -> bool {
    matches!(
        name,
        "build" | "out" | "target" | ".gradle" | ".git" | ".idea" | "node_modules" | ".kotlin"
    ) || name.starts_with(".kotlin-analyzer")
}

pub(crate) fn discover(root: &Path) -> ProjectModel {
    let mut module_directories: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| {
            let is_root = entry.depth() == 0;
            let name = entry.file_name().to_string_lossy();
            is_root || !(entry.file_type().is_dir() && is_excluded_directory(&name))
        })
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.file_type().is_file() && (name == "build.gradle" || name == "build.gradle.kts")
        })
        .filter_map(|entry| entry.path().parent().map(Path::to_path_buf))
        .collect();
    if module_directories.is_empty() {
        module_directories.push(root.to_path_buf());
    }
    module_directories.sort();
    module_directories.dedup();

    let mut modules = Vec::new();
    let mut source_sets: Vec<SourceSet> = Vec::new();
    for directory in module_directories {
        let module_id = ModuleId(modules.len() as u32);
        let mut ids = Vec::new();
        for (name, directories) in source_sets_by_convention(&directory) {
            let id = SourceSetId(source_sets.len() as u32);
            ids.push(id);
            source_sets.push(SourceSet {
                id,
                module: module_id,
                kind: SourceSetKind::of_name(&name),
                platform: Platform::of_name(&name),
                name,
                directories,
                dependencies: Vec::new(),
                libraries: Vec::new(),
                classpath: Classpath::Complete,
                compiler: CompilerOptions::default(),
            });
        }
        modules.push(Module {
            gradle_path: gradle_path(root, &directory),
            directory,
            source_sets: ids,
        });
    }
    // Without the build's dependency graph, every source set sees every
    // main source set, and tests also see every test-fixtures one — except
    // that Kotlin Multiplatform's common code sees only common code, not
    // the platform source sets (`jvmMain`, `iosMain`) built on it.
    let kinds: Vec<(SourceSetId, SourceSetKind, bool)> = source_sets
        .iter()
        .map(|source_set| (source_set.id, source_set.kind, is_common(&source_set.name)))
        .collect();
    for source_set in &mut source_sets {
        let sees_fixtures = source_set.kind == SourceSetKind::Test;
        let sees_only_common = is_common(&source_set.name);
        source_set.dependencies = kinds
            .iter()
            .filter(|(id, _, _)| *id != source_set.id)
            .filter(|(_, _, common)| *common || !sees_only_common)
            .filter(|(_, kind, _)| {
                *kind == SourceSetKind::Main
                    || (sees_fixtures && *kind == SourceSetKind::TestFixtures)
            })
            .map(|(id, _, _)| *id)
            .collect();
    }
    ProjectModel::new(
        root.to_path_buf(),
        ModelOrigin::DirectoryConvention,
        modules,
        source_sets,
        None,
    )
}

/// Kotlin Multiplatform's common source sets: `commonMain`, `commonTest`.
fn is_common(name: &str) -> bool {
    name.starts_with("common")
}

/// `src/<name>/{kotlin,java}` directories of a module, by source set name.
fn source_sets_by_convention(module_directory: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let Ok(entries) = std::fs::read_dir(module_directory.join("src")) else {
        return Vec::new();
    };
    let mut found: Vec<(String, Vec<PathBuf>)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let directories: Vec<PathBuf> = ["kotlin", "java"]
                .iter()
                .map(|language| entry.path().join(language))
                .filter(|directory| directory.is_dir())
                .collect();
            (!directories.is_empty()).then_some((name, directories))
        })
        .collect();
    // `main` first, so it gets the lowest id of its module.
    found.sort_by_key(|(name, _)| (name != "main", name.clone()));
    found
}

fn gradle_path(root: &Path, module_directory: &Path) -> String {
    let relative = module_directory
        .strip_prefix(root)
        .unwrap_or(module_directory);
    let segments: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    format!(":{}", segments.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn modules_and_source_sets_follow_directory_conventions() {
        let root = std::env::temp_dir().join(format!("ka-convention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch(&root.join("settings.gradle.kts"));
        touch(&root.join("api/build.gradle.kts"));
        touch(&root.join("api/src/main/kotlin/A.kt"));
        touch(&root.join("api/src/test/kotlin/ATest.kt"));
        touch(&root.join("api/build/generated/B.kt"));
        touch(&root.join("core/build.gradle"));
        touch(&root.join("core/src/main/java/C.java"));

        let model = discover(&root);
        let paths: Vec<&str> = model
            .modules
            .iter()
            .map(|m| m.gradle_path.as_str())
            .collect();
        assert_eq!(paths, vec![":api", ":core"]);
        let api_test = model
            .source_set_of(&root.join("api/src/test/kotlin/ATest.kt"))
            .unwrap();
        let core_main = model
            .source_set_of(&root.join("core/src/main/java/C.java"))
            .unwrap();
        let api_main = model
            .source_set_of(&root.join("api/src/main/kotlin/A.kt"))
            .unwrap();
        assert_eq!(model.source_set(api_test).kind, SourceSetKind::Test);
        assert!(model.visible_source_sets(api_main).contains(&core_main));
        assert!(!model.visible_source_sets(api_main).contains(&api_test));
        assert_eq!(
            model.source_set_of(&root.join("api/build/generated/B.kt")),
            None
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn multiplatform_and_android_source_sets_are_main_or_test_by_name() {
        let root = std::env::temp_dir().join(format!("ka-convention-kmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        touch(&root.join("settings.gradle.kts"));
        touch(&root.join("shared/build.gradle.kts"));
        let sets = [
            ("commonMain", SourceSetKind::Main),
            ("iosMain", SourceSetKind::Main),
            ("commonTest", SourceSetKind::Test),
            ("androidUnitTest", SourceSetKind::Test),
            ("demo", SourceSetKind::Main),
            ("testDemo", SourceSetKind::Test),
            ("androidTest", SourceSetKind::Test),
            ("testFixtures", SourceSetKind::TestFixtures),
        ];
        for (name, _) in sets {
            touch(&root.join(format!("shared/src/{name}/kotlin/F.kt")));
        }

        let model = discover(&root);
        let set_of = |name: &str| {
            model
                .source_set_of(&root.join(format!("shared/src/{name}/kotlin/F.kt")))
                .unwrap()
        };
        for (name, kind) in sets {
            assert_eq!(model.source_set(set_of(name)).kind, kind, "{name}");
        }
        let ios_sees = model.visible_source_sets(set_of("iosMain"));
        assert!(ios_sees.contains(&set_of("commonMain")));
        assert!(!ios_sees.contains(&set_of("commonTest")));
        let common_test_sees = model.visible_source_sets(set_of("commonTest"));
        assert!(common_test_sees.contains(&set_of("commonMain")));
        // Common code doesn't see the platforms built on it.
        let common_sees = model.visible_source_sets(set_of("commonMain"));
        assert!(!common_sees.contains(&set_of("iosMain")));
        assert!(!common_test_sees.contains(&set_of("demo")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
