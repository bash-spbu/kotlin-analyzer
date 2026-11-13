# kotlin-analyzer

A Kotlin language server for JVM projects (Gradle, Spring and the like),
written in Rust and designed after
[rust-analyzer](https://github.com/rust-lang/rust-analyzer): an incremental
database of queries over the workspace, a semantic model in front of it, and
editor features as pure functions of a snapshot.

Kotlin gets the full feature set. Java — in the workspace, the JDK and
libraries — is read for navigation, hover and types.

**Status:** early development. Working so far (milestones M1–M2): the
project model from Gradle; declarations of every Kotlin and Java file and of
every library on the classpath (read straight from class files and Kotlin
metadata, the JDK from `ct.sym`); name resolution of types per module through
sources and libraries; the LSP requests `documentSymbol`, `workspace/symbol`,
`definition` (types and imports, into sources JARs and the JDK's `src.zip`)
and `hover` (signature and KDoc/Javadoc), with syntax diagnostics. See `docs/spec/` for
what it is being built to do.

## Project model

Export the build's model once (and after dependency changes):

```bash
./gradlew --init-script /path/to/kotlin-analyzer/editors/gradle/kotlin-analyzer.init.gradle kotlinAnalyzerModel
```

Without it, modules and source sets are guessed from directory layout.

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
