# kotlin-analyzer for Zed

Kotlin support for Zed in one extension:

- **the Kotlin language** — `.kt` and `.kts` files: highlighting,
  outline and breadcrumbs, bracket matching, auto-indentation, comment
  toggling, Vim text objects (`af`/`if`, `ac`/`ic`). The grammar is
  [fwcd/tree-sitter-kotlin](https://github.com/fwcd/tree-sitter-kotlin)
  (MIT), which Zed fetches and builds at the pinned revision — the one
  Zed's Java extension uses for Gradle Kotlin scripts, since grammar names
  are global in Zed. The queries in `languages/kotlin/` are this project's;
  `crates/syntax/tests/zed_queries.rs` checks them;
- **the kotlin-analyzer language server**, for Kotlin and (read-only) Java.
  Java's own language (grammar) comes from Zed's Java extension.

No other Kotlin extension is needed; remove any that defines the Kotlin
language too, or Zed will pick one of the two.

## Setup

1. In Zed: *zed: install dev extension* → this directory (after changes:
   *Rebuild* on the extension's card).
2. In `settings.json`, put the server first:

   ```json
   "languages": {
     "Kotlin": { "language_servers": ["kotlin-analyzer", "!kotlin-lsp", "!kotlin-language-server", "..."] },
     "Java": { "language_servers": ["kotlin-analyzer", "!jdtls", "..."] }
   }
   ```

The server binary is the first of:

1. `lsp.kotlin-analyzer.binary.path`;
2. `kotlin-analyzer` on the worktree's `PATH` (e.g. from
   `cargo install --path crates/kotlin-analyzer`);
3. the release the extension is built for (`SERVER_VERSION` in
   `src/release.rs`), downloaded from GitHub on first use into the
   extension's work directory, with its SHA-256 checked. Earlier versions'
   downloads are removed. Releases exist for macOS and Linux on arm64 and
   x86_64.

## Settings

Under `lsp.kotlin-analyzer`:

- `binary.path` (and `arguments`, `env`): a binary instead of the one on
  the PATH or the download;
- `initialization_options`: `{ "diagnostics": { "disabled": ["unused-import"], "experimental": false }, "gradleExport": true, "projectJson": "/path/to/project.json" }`.

On opening a Gradle build, the server exports the build's model (modules,
source sets, exact libraries, JDK) in the background with its bundled init
script, into its cache — nothing is written to the project except Gradle's
own `build/` output. Until that finishes, it works from the directory layout.

Semantic tokens (`"semantic_tokens": "combined"` per language) add what the
grammar can't tell: properties vs. locals vs. parameters, with the modifiers
`declaration`, `readonly`, `static`, `deprecated` and `mutable` (a `var`) for
`semantic_token_rules`.

## Tests

`cargo test` (on the host) checks the release's asset names and that a
download is unpacked only when its checksum agrees. A build with
`KOTLIN_ANALYZER_DOWNLOADS=<base>` downloads `<base>/v<version>/<asset>`
instead of GitHub's, for trying the download against a local release.
