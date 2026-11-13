# kotlin-analyzer for Zed

Registers the `kotlin-analyzer` language server for Kotlin and Java. The
languages (grammars, highlighting) come from Zed's own Kotlin and Java
extensions; this one only adds the server.

1. Install the server: `cargo install --path crates/kotlin-analyzer` (from
   the repository root), so `kotlin-analyzer` is on the PATH.
2. In Zed: *zed: install dev extension* → this directory.
3. Install the **Kotlin** (and, for Java, **Java**) extension from Zed's
   registry, if not installed.
4. In `settings.json`, put the server first for the languages:

   ```json
   "languages": {
     "Kotlin": { "language_servers": ["kotlin-analyzer", "!kotlin-lsp", "!kotlin-language-server", "..."] },
     "Java": { "language_servers": ["kotlin-analyzer", "!jdtls", "..."] }
   }
   ```

Settings, under `lsp.kotlin-analyzer`:

- `binary.path` (and `arguments`, `env`): a binary other than the one on
  the PATH;
- `initialization_options`: `{ "diagnostics": { "disabled": ["unused-import"], "experimental": false }, "gradleExport": true, "projectJson": "/path/to/project.json" }`.

On opening a Gradle build, the server exports the build's model (modules,
source sets, exact libraries, JDK) in the background with its bundled init
script, into its cache — nothing is written to the project except Gradle's
own `build/` output. Until that finishes, it works from the directory layout.
