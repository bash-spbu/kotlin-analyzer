# kotlin-analyzer

A Kotlin language server for JVM projects (Gradle, Spring and the like),
written in Rust and designed after
[rust-analyzer](https://github.com/rust-lang/rust-analyzer): an incremental
database of queries over the workspace, a semantic model in front of it, and
editor features as pure functions of a snapshot.

Kotlin gets the full feature set. Java — in the workspace, the JDK and
libraries — is read for navigation, hover and types.

**Status:** milestones M1–M6 of the plan are in.

- **Model:** the project model from Gradle, exported by the server itself.
- **Declarations and resolution:** declarations of every Kotlin and Java
  file and of every library on the classpath, read straight from class files
  and Kotlin metadata; the JDK comes from `ct.sym`. Names resolve per module,
  through sources and libraries.
- **Bodies:** type inference in bodies — calls and overloads, generics,
  lambdas, smart casts.
- **LSP features:**
  - definition (into sources JARs and the JDK's `src.zip`), hover, inlay type
    hints;
  - completion with auto-import, signature help;
  - references and rename;
  - semantic tokens, folding, document and workspace symbols;
  - diagnostics with quick fixes: syntax, missing imports, argument count,
    unsafe calls on nullable receivers, non-exhaustive `when`, unused imports,
    missing package.

See `docs/spec/` for what each does, and when it stays silent.

## Using it in Zed

1. `cargo install --path crates/kotlin-analyzer` puts `kotlin-analyzer` on
   the PATH.
2. *zed: install dev extension* → `editors/zed` (see its README for
   settings). It provides the Kotlin language as well — highlighting,
   outline, brackets, indentation — so no other Kotlin extension is needed;
   Java's grammar comes from Zed's Java extension.

## Project model

On opening a Gradle build, the server runs the bundled init script in the
background and keeps the result in its cache. To export by hand instead
(for example, to commit it or point several tools at it):

```bash
./gradlew --init-script /path/to/kotlin-analyzer/editors/gradle/kotlin-analyzer.init.gradle kotlinAnalyzerModel
```

which writes `.kotlin-analyzer/project.json`. Until a model is there,
modules and source sets are guessed from the directory layout.

## Layout

| crate | what it holds |
|---|---|
| `crates/vfs` | file ids, file contents and open-editor overlays, line index |
| `crates/syntax` | Kotlin / Java syntax trees and their typed view |
| `crates/project-model` | Gradle modules, source sets, classpaths, the JDK |
| `crates/base-db` | the incremental database's inputs |
| `crates/hir-def` | item trees, per-module definition maps, body scopes |
| `crates/hir-ty` | types, members, inference |
| `crates/hir` | the semantic API features use |
| `crates/ide` | editor features and diagnostics |
| `crates/classfile` | compiled classes: JARs, class files, JVM signatures, Kotlin metadata |
| `crates/kotlin-analyzer` | the server binary and its measuring tools |

## Building

```bash
cargo build --release
cargo xtask check   # layering rule, fmt, clippy, tests
```

## License

Apache License 2.0 — see [LICENSE](LICENSE).
