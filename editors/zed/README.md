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

1. Install the server: `cargo install --path crates/kotlin-analyzer` (from
   the repository root), so `kotlin-analyzer` is on the PATH.
2. In Zed: *zed: install dev extension* → this directory (after changes:
   *Rebuild* on the extension's card).
3. In `settings.json`, put the server first:

   ```json
   "languages": {
     "Kotlin": { "language_servers": ["kotlin-analyzer", "!kotlin-lsp", "!kotlin-language-server", "..."] },
     "Java": { "language_servers": ["kotlin-analyzer", "!jdtls", "..."] }
   }
   ```

## Settings

Under `lsp.kotlin-analyzer`:

- `binary.path` (and `arguments`, `env`): a binary other than the one on
  the PATH;
- `initialization_options`: `{ "diagnostics": { "disabled": ["unused-import"], "experimental": false }, "gradleExport": true, "projectJson": "/path/to/project.json" }`.

On opening a Gradle build, the server exports the build's model (modules,
source sets, exact libraries, JDK) in the background with its bundled init
script, into its cache — nothing is written to the project except Gradle's
own `build/` output. Until that finishes, it works from the directory layout.

Semantic tokens (`"semantic_tokens": "combined"` per language) add what the
grammar can't tell: properties vs. locals vs. parameters, with the modifiers
`declaration`, `readonly`, `static`, `deprecated` and `mutable` (a `var`) for
`semantic_token_rules`.
