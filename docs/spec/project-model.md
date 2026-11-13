# Project model

## Workspace root

The root is the client's `rootUri` (or first workspace folder). If that is
inside a Gradle build, the build's root (the directory with
`settings.gradle(.kts)`) is used.

## Modules and source sets

A **module** is a Gradle project (`:payments:api`). Each module has **source
sets**; the ones that matter:

| source set | sees | seen by |
|---|---|---|
| `main` | its own main, main of modules it depends on, its classpath | everything in the module, dependent modules |
| `testFixtures` | main | test, dependents' tests that declare it |
| `test` (and other test-like: `integrationTest`, …) | main, testFixtures, test classpath | nothing else |

Each source set lists its **source directories**, including generated ones
(OpenAPI / protobuf / KSP / kapt output under `build/`). Generated directories
are ordinary source directories that happen to be produced by the build.

## Where the model comes from

1. **Gradle init script** (shipped in `editors/gradle/`): a task the user runs
   (or the server runs on request) writes `.kotlin-analyzer/project.json` in the
   root:
   ```json
   {
     "version": 1,
     "jdk": { "home": "/path/to/jdk", "release": 21 },
     "modules": [
       {
         "path": ":payments:api",
         "directory": "payments/api",
         "sourceSets": [
           {
             "name": "main",
             "kind": "main",
             "sourceDirectories": ["src/main/kotlin", "build/generated-sources/openapi"],
             "moduleDependencies": [{ "module": ":common", "sourceSet": "main" }],
             "libraries": [
               { "coordinates": "org.springframework:spring-core:6.2.1",
                 "jar": "/…/spring-core-6.2.1.jar",
                 "sourcesJar": "/…/spring-core-6.2.1-sources.jar" }
             ]
           }
         ]
       }
     ]
   }
   ```
   Paths are relative to the root unless absolute. Libraries carry exactly the
   versions the build resolves for that source set — never "the newest in the
   Gradle cache".
   When neither `projectJson` nor `.kotlin-analyzer/project.json` is given
   and the root is a Gradle build, **the server runs the export itself**:
   the init script is bundled in the binary, runs in the background
   (`./gradlew` when the build has a wrapper, else `gradle`) and writes the
   model to the cache (`$XDG_CACHE_HOME/kotlin-analyzer/projects/<root>-<hash>/project.json`),
   so nothing lands in the project besides Gradle's own `build/` output.
   The workspace loads by convention meanwhile and reloads when the export
   finishes. The export is redone when a build file (`build.gradle(.kts)`,
   `settings.gradle(.kts)`, `gradle.properties`, `libs.versions.toml`) is
   newer than the cached model. A failed export is reported with
   `window/showMessage`. `gradleExport: false` turns this off.
2. **Fallback by convention** when the file is missing: every directory with a
   `build.gradle(.kts)` is a module; `src/<set>/kotlin` and `src/<set>/java`
   are source directories of source set `<set>`; no module dependencies are
   known, so every module sees every other module's main code; libraries are
   unknown (navigation into libraries is limited, diagnostics that need
   library completeness stay silent).

The server watches `project.json`; when it changes the model is reloaded.

## Settings

`initializationOptions`:
- `projectJson`: where the exported model is, when not at
  `.kotlin-analyzer/project.json` in the root;
- `gradleExport` (default `true`): let the server export the Gradle model
  when none is given;
- `sourcePaths`: extra source directories (each becomes its own module);
- `ignorePatterns`: gitignore-style patterns of files not to analyse;
- `diagnostics.disabled`: names of checks to turn off; `diagnostics.experimental`:
  run experimental checks.

## Files

`.kt`, `.kts` (Kotlin) and `.java` (Java, read-only analysis). Everything
under `build/`, `.gradle/`, `.git/`, `node_modules/`, `out/` is ignored unless
it is a listed source directory.
