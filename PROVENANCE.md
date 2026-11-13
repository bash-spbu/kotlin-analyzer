# Provenance

kotlin-analyzer is a new implementation. Its author, Alexander Bashkirov,
earlier worked on a private fork of kmp-lsp (MIT, by Juraj Oceliak) and wrote
parts of it himself. This file lists everything here that derives from that
earlier work, so that what is new and what was carried over stays clear.

Rules:
- Only work authored by Alexander Bashkirov may be carried over (verified with
  `git blame` in the fork). Nothing written by other kmp-lsp contributors is.
- Carried-over work is rewritten against this project's own APIs; the entry
  says what was taken (design, algorithm, tests) and from where.
- kmp-lsp is otherwise used only as a running binary, to compare behaviour.

## Carried over

| here | from (author's own work in the fork) | what was taken |
|---|---|---|
| `editors/gradle/kotlin-analyzer.init.gradle` | `contrib/gradle/kmp-lsp-deps.init.gradle` (author's own) | the idea of an init-script task exporting the build's resolved dependencies and generated source roots; rewritten for the new `project.json` model (modules, source sets, module dependencies, per-source-set libraries with sources JARs, JDK) and Gradle 9's per-project resolution |
| `crates/hir-ty/src/infer/` | `src/analysis/` of the fork (scope-tower call resolution, call receivers, lambda parameters — the author's own) | the approach only: resolve a call against candidate levels in Kotlin's scope order and type lambda parameters from the parameter they're passed to. The rules themselves are taken from the Kotlin specification (overload resolution, type inference, smart casts); no code was read or reused |
| `crates/syntax/src/pointer.rs` (`SyntaxNodePtr`) | `src/indexer/file_model/syntax_ptr.rs` (author's own) | the design: a node remembered by kind and range, found again by climbing from the smallest node spanning the range; rewritten |

## Formats implemented from published specifications

| here | specification | notes |
|---|---|---|
| `crates/classfile/src/class_file.rs`, `signature.rs` | The Java Virtual Machine Specification, chapter 4 (class file format, descriptors, generic signatures) | |
| `crates/classfile/src/kotlin.rs` | Kotlin metadata, as published in JetBrains' Kotlin repository (Apache-2.0): `metadata.proto`, `jvm_metadata.proto`, `builtins.proto`, the flag layout of `Flags.java`, the byte packing of `BitEncoding`, the string table of `JvmNameResolverBase` | Implemented from the format definitions; the table of predefined strings (`PREDEFINED_STRINGS`) is format data, transcribed as is. |

## Third-party code used as dependencies

| what | license | how |
|---|---|---|
| tree-sitter, tree-sitter-java, brokk-tree-sitter-kotlin | MIT | crates.io dependencies |
| salsa, lsp-server, lsp-types, zip, rayon, serde, … | MIT / Apache-2.0 | crates.io dependencies (see `Cargo.lock`) |
| `LICENSE` text | Apache-2.0 | the license's own text |
