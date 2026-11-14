//! `.kotlin-analyzer/project.json`, as the Gradle init script writes it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{
    Classpath, Jdk, Library, LoadError, ModelOrigin, Module, ModuleId, Platform, ProjectModel,
    SourceSet, SourceSetId, SourceSetKind,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectFile {
    version: u32,
    jdk: Option<JdkEntry>,
    modules: Vec<ModuleEntry>,
}

#[derive(Deserialize)]
struct JdkEntry {
    home: PathBuf,
    release: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleEntry {
    path: String,
    directory: PathBuf,
    source_sets: Vec<SourceSetEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceSetEntry {
    name: String,
    kind: Option<String>,
    platform: Option<String>,
    classpath: Option<String>,
    source_directories: Vec<PathBuf>,
    #[serde(default)]
    module_dependencies: Vec<ModuleDependencyEntry>,
    #[serde(default)]
    libraries: Vec<LibraryEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleDependencyEntry {
    module: String,
    source_set: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryEntry {
    coordinates: Option<String>,
    jar: PathBuf,
    sources_jar: Option<PathBuf>,
}

const SUPPORTED_VERSION: u32 = 1;

pub(crate) fn parse(root: &Path, text: &str) -> Result<ProjectModel, LoadError> {
    let file: ProjectFile =
        serde_json::from_str(text).map_err(|error| LoadError::Malformed(error.to_string()))?;
    if file.version != SUPPORTED_VERSION {
        return Err(LoadError::Malformed(format!(
            "version {} (this kotlin-analyzer reads version {SUPPORTED_VERSION})",
            file.version
        )));
    }
    let absolute = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        }
    };
    let mut modules = Vec::new();
    let mut source_sets = Vec::new();
    // (source set, its declared dependencies), resolved once all are known.
    let mut pending_dependencies = Vec::new();
    for (module_index, module) in file.modules.iter().enumerate() {
        let module_id = ModuleId(module_index as u32);
        let mut ids = Vec::new();
        for source_set in &module.source_sets {
            let id = SourceSetId(source_sets.len() as u32);
            ids.push(id);
            let kind = match source_set.kind.as_deref() {
                Some("main") => SourceSetKind::Main,
                Some("testFixtures") => SourceSetKind::TestFixtures,
                Some("test") => SourceSetKind::Test,
                _ => SourceSetKind::of_name(&source_set.name),
            };
            let platform = match source_set.platform.as_deref() {
                Some("jvm") => Platform::Jvm,
                Some("other") => Platform::NotJvm,
                _ => Platform::of_name(&source_set.name),
            };
            source_sets.push(SourceSet {
                id,
                module: module_id,
                name: source_set.name.clone(),
                kind,
                platform,
                classpath: match source_set.classpath.as_deref() {
                    Some("partial") => Classpath::Partial,
                    _ => Classpath::Complete,
                },
                directories: source_set
                    .source_directories
                    .iter()
                    .map(|directory| absolute(directory))
                    .collect(),
                dependencies: Vec::new(),
                libraries: source_set
                    .libraries
                    .iter()
                    .map(|library| Library {
                        coordinates: library.coordinates.clone(),
                        jar: absolute(&library.jar),
                        sources_jar: library.sources_jar.as_deref().map(absolute),
                    })
                    .collect(),
            });
            pending_dependencies.push((id, &source_set.module_dependencies));
        }
        modules.push(Module {
            gradle_path: module.path.clone(),
            directory: absolute(&module.directory),
            source_sets: ids,
        });
    }
    let find = |module_path: &str, source_set_name: &str| -> Option<SourceSetId> {
        let module = modules
            .iter()
            .find(|module| module.gradle_path == module_path)?;
        module
            .source_sets
            .iter()
            .copied()
            .find(|id| source_sets[id.0 as usize].name == source_set_name)
    };
    let mut resolved = Vec::new();
    for (id, dependencies) in pending_dependencies {
        let mut targets: Vec<SourceSetId> = dependencies
            .iter()
            .filter_map(|dependency| find(&dependency.module, &dependency.source_set))
            .filter(|target| *target != id)
            .collect();
        // A test source set always sees its own module's `main` and
        // fixtures, whether or not the build lists them. (Kotlin
        // Multiplatform and Android source sets list what they see.)
        let own = &source_sets[id.0 as usize];
        if own.kind != SourceSetKind::Main {
            let module = &modules[own.module.0 as usize];
            for sibling in &module.source_sets {
                let sibling_set = &source_sets[sibling.0 as usize];
                let is_main = sibling_set.kind == SourceSetKind::Main && sibling_set.name == "main";
                let is_fixtures = sibling_set.kind == SourceSetKind::TestFixtures;
                let below = match own.kind {
                    SourceSetKind::Test => is_main || is_fixtures,
                    _ => is_main,
                };
                if below && !targets.contains(sibling) && *sibling != id {
                    targets.push(*sibling);
                }
            }
        }
        resolved.push((id, targets));
    }
    for (id, targets) in resolved {
        source_sets[id.0 as usize].dependencies = targets;
    }
    let jdk = file.jdk.map(|jdk| Jdk {
        home: jdk.home,
        release: jdk.release,
    });
    Ok(ProjectModel::new(
        root.to_path_buf(),
        ModelOrigin::GradleExport,
        modules,
        source_sets,
        jdk,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "version": 1,
      "jdk": { "home": "/jdk", "release": 21 },
      "modules": [
        { "path": ":common", "directory": "common", "sourceSets": [
            { "name": "main", "kind": "main", "sourceDirectories": ["common/src/main/kotlin"] }
        ]},
        { "path": ":api", "directory": "api", "sourceSets": [
            { "name": "main", "kind": "main",
              "sourceDirectories": ["api/src/main/kotlin", "api/build/generated-sources/openapi"],
              "moduleDependencies": [{ "module": ":common", "sourceSet": "main" }],
              "libraries": [{ "coordinates": "g:a:1", "jar": "/m2/a-1.jar", "sourcesJar": "/m2/a-1-sources.jar" }] },
            { "name": "test", "kind": "test", "sourceDirectories": ["api/src/test/kotlin"],
              "moduleDependencies": [{ "module": ":common", "sourceSet": "main" }] }
        ]}
      ]
    }"#;

    #[test]
    fn multiplatform_source_sets_see_what_they_list_and_their_own_main_only_by_name() {
        let text = r#"{
          "version": 1,
          "modules": [
            { "path": ":shared", "directory": "shared", "sourceSets": [
                { "name": "commonMain", "kind": "main", "sourceDirectories": ["shared/src/commonMain/kotlin"] },
                { "name": "jvmMain", "kind": "main", "sourceDirectories": ["shared/src/jvmMain/kotlin"],
                  "moduleDependencies": [{ "module": ":shared", "sourceSet": "commonMain" }] },
                { "name": "jsMain", "kind": "main", "platform": "other", "classpath": "partial", "sourceDirectories": ["shared/src/jsMain/kotlin"],
                  "moduleDependencies": [{ "module": ":shared", "sourceSet": "commonMain" }] },
                { "name": "jvmTest", "kind": "test", "sourceDirectories": ["shared/src/jvmTest/kotlin"],
                  "moduleDependencies": [{ "module": ":shared", "sourceSet": "jvmMain" },
                                         { "module": ":shared", "sourceSet": "commonMain" },
                                         { "module": ":gone", "sourceSet": "main" }] }
            ]}
          ]
        }"#;
        let model = parse(Path::new("/work"), text).unwrap();
        let [common, jvm, js, jvm_test] = model.modules[0].source_sets[..] else {
            panic!("four source sets");
        };
        assert_eq!(model.source_set(jvm_test).dependencies, vec![jvm, common]);
        assert!(!model.visible_source_sets(jvm_test).contains(&js));
        assert_eq!(model.source_set(js).platform, Platform::NotJvm);
        assert_eq!(model.source_set(js).classpath, Classpath::Partial);
        assert_eq!(model.source_set(jvm).classpath, Classpath::Complete);
        // Without a platform written, the name decides.
        assert_eq!(model.source_set(jvm).platform, Platform::Jvm);
    }

    #[test]
    fn modules_source_sets_and_dependencies_are_read() {
        let model = parse(Path::new("/work"), SAMPLE).unwrap();
        assert_eq!(model.origin, ModelOrigin::GradleExport);
        assert_eq!(model.modules.len(), 2);
        let api_main = model.modules[1].source_sets[0];
        let api_test = model.modules[1].source_sets[1];
        let common_main = model.modules[0].source_sets[0];
        assert_eq!(model.source_set(api_main).dependencies, vec![common_main]);
        // The test source set sees its own module's main without listing it.
        assert_eq!(
            model.source_set(api_test).dependencies,
            vec![common_main, api_main]
        );
        assert_eq!(
            model.source_set_of(Path::new(
                "/work/api/build/generated-sources/openapi/a/B.kt"
            )),
            Some(api_main)
        );
        assert_eq!(model.source_set_of(Path::new("/work/README.md")), None);
        let library = &model.source_set(api_main).libraries[0];
        assert_eq!(
            library.sources_jar.as_deref(),
            Some(Path::new("/m2/a-1-sources.jar"))
        );
        assert_eq!(model.jdk.as_ref().unwrap().release, Some(21));
    }

    #[test]
    fn main_does_not_see_its_tests() {
        let model = parse(Path::new("/work"), SAMPLE).unwrap();
        let api_main = model.modules[1].source_sets[0];
        let api_test = model.modules[1].source_sets[1];
        assert!(!model.visible_source_sets(api_main).contains(&api_test));
        assert!(model.visible_source_sets(api_test).contains(&api_main));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let result = parse(Path::new("/work"), r#"{"version": 9, "modules": []}"#);
        assert!(matches!(result, Err(LoadError::Malformed(_))));
    }
}
