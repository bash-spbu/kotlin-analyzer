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

   The init script reads three kinds of module:
   - **JVM** — the `java` plugin's source sets (Java, Kotlin/JVM, Spring…);
     a dependency on a Kotlin Multiplatform module reaches its `commonMain`
     and `jvmMain`.
   - **Kotlin Multiplatform** — `kotlin.sourceSets` with `kind` from the
     compilations they're part of (only test compilations: `test`). A source
     set sees what it `dependsOn` (transitively), a test also the main code
     *every* one of its compilations tests (`commonTest` → `commonMain`,
     `jvmTest` → `jvmMain` and `commonMain`), and the projects it declares
     (`api`/`implementation`/`compileOnly`), as their same-named, `commonMain`
     and `main` source sets. Libraries: the classpath of a JVM or Android
     compilation the source set is part of (its own one first), so
     `commonMain` gets the JVM's; a source set built only for Native or JS
     gets the standard library alone (their klibs aren't read).
   - **Android** (AGP) — `android.sourceSets`. One debug variant stands for
     the module (`debug`, else the first `<flavours>Debug`): its source sets
     (`main`, the flavours', the build type's) see one another, tests
     (`test…`, `androidTest…`) see them, another flavour or build type sees
     `main` only. Libraries: the variant's compile classpaths (main, unit
     test, device test) with Android archives read as their classes JARs, and
     the SDK's `android.jar`.

   Each source set also says its `platform` — `jvm`, or `other` for code
   built only for JS, Wasm or Native: only JVM code (Android's, and common
   code built for the JVM too) sees the JDK. Without it the name decides
   (`jsMain`, `iosArm64Test`, `nativeMain`, `webMain` are `other`). A
   `classpath` the build only partly resolved (a library that couldn't be
   fetched, a toolchain that isn't installed) is `partial`: names that
   would come from it are never called missing. The JDK is read from
   `ct.sym` for the release asked for, or the newest one before it that
   `ct.sym` describes in full (JDK 21's own release is in its runtime image,
   not its `ct.sym`). The release is the root project's toolchain's, else
   the one every JVM source set compiles against (below) when they agree;
   else the JDK the build runs on is read, and what it alone says (a
   deprecation) isn't reported.

   Each source set also says its `compilerOptions`, read from the Kotlin
   compile task(s) it's part of: `warningsAsErrors` (`allWarningsAsErrors`,
   `-Werror`), `suppressWarnings` (`-nowarn`), `warningLevels` by the
   compiler's warning name (`-Xsuppress-warning=NAME` is `disabled`,
   `-Xwarning-level=NAME:error|warning|disabled`), `optIn`, `apiVersion`
   (unset, the plugin's version: `2.3`) and `jdkRelease` (the task's Java
   toolchain, or `-Xjdk-release`). A source set in several compilations
   (Kotlin Multiplatform) gets them merged: warnings are errors if any
   compilation says so; suppressed, opted into or of one level only if all
   agree; a version only if all have the same.

   A source set with an explicit `kind` and dependencies (all of these) sees
   only what it lists; the implicit "a test sees its module's `main` and
   test fixtures" applies to source sets named `main` and `testFixtures`.
   Artifacts that don't resolve (offline, a failed download) are left out
   and reported as a Gradle warning.
   When neither `projectJson` nor `.kotlin-analyzer/project.json` is given
   and the root is a Gradle build, **the server runs the export itself**:
   the init script is bundled in the binary, runs in the background
   (`./gradlew` when the build has a wrapper, else `gradle`) and writes the
   model to the cache (`$XDG_CACHE_HOME/kotlin-analyzer/projects/<root>-<hash>/project.json`),
   so nothing lands in the project besides Gradle's own `build/` output.
   The export runs with `--no-configuration-cache` and isolated projects off
   (its tasks read projects while they run).
   The workspace loads by convention meanwhile and reloads when the export
   finishes. The export is redone when the build files
   (`build.gradle(.kts)`, `settings.gradle(.kts)`, `gradle.properties`,
   `libs.versions.toml`) changed since it was made: the newest one's
   modification time is recorded next to the model (`project.stamp`) and
   compared — at startup and on reload — off the main thread. (Comparing
   with the model's own time would export forever after a build file
   stamped in the future.) A failed export is reported with
   `window/showMessage`. `gradleExport: false` turns this off.
2. **Fallback by convention** when the file is missing: every directory with a
   `build.gradle(.kts)` is a module; `src/<set>/kotlin` and `src/<set>/java`
   are source directories of source set `<set>`; no module dependencies are
   known, so every module sees every other module's main code; libraries are
   unknown (navigation into libraries is limited, diagnostics that need
   library completeness stay silent). A source set is a test one when its
   name says so — `test…` (`testDebug`), `androidTest…`, `…Test`
   (`integrationTest`, `commonTest`, `androidUnitTest`) — and main code
   otherwise (`main`, Kotlin Multiplatform's `commonMain`/`jvmMain`/`iosMain`,
   Android's build types and flavours). Kotlin Multiplatform's common source
   sets (`common…`) see only common code, never the platform source sets
   built on them.



`workspace/executeCommand` `kotlin-analyzer.reloadWorkspace` re-exports the
model if the build files changed and reads the workspace again: new files
appear, files gone from disk or from the model are dropped, and every file
is placed by the new model (source set, package directory) even where its
text didn't change. A load started while another runs supersedes it.
*(Later: watching the files and the model instead of a command.)*

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
