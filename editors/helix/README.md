# kotlin-analyzer for Helix

Helix already ships the Kotlin language: grammar, highlighting, text objects
and indentation. Setting it up here only adds the language server, for Kotlin
and (read-only) Java.

## Install

```bash
editors/helix/install.sh
```

The script does two things:

1. If `kotlin-analyzer` isn't on the `PATH`, it installs the latest release
   into `~/.local/bin`. See `editors/install-server.sh`: macOS and Linux,
   arm64 and x86_64, with the SHA-256 checked.
2. It appends [`languages.toml`](languages.toml) to
   `~/.config/helix/languages.toml`. If that file already configures Kotlin,
   Java or kotlin-analyzer, it changes nothing and asks you to merge by hand.

Options:

- `--no-java`: leave Java to another server (jdtls).
- `--directory <dir>`: put the binary somewhere else.

Check the result with `hx --health kotlin`.

Open a file inside a Gradle build. Helix starts the server in the outermost
directory with a `settings.gradle(.kts)` or `build.gradle(.kts)`, up to the
version-control root. The server then exports the build's model in the
background.

## Gradle

Helix has no tasks, so `install.sh` binds keys to `:sh` in
`~/.config/helix/config.toml` (from [`config.toml`](config.toml)), unless
`Space G` is taken already. The output shows in a popup once Gradle is done.

- `Space G b` runs `gradle build -x test`.
- `Space G c` runs `gradle clean`.
- `Space G t` runs `gradle test --rerun-tasks` in the current file's project
  and its subprojects. From a parent's `build.gradle.kts`, it tests the
  parent's group.

`install.sh` copies the script to
`~/.local/share/kotlin-analyzer/gradle_task.py`. The project holding the file is its nearest ancestor directory with a
`build.gradle(.kts)`, and the build's root is the nearest one with a
`settings.gradle(.kts)`. Gradle is the root's `gradlew`, else `gradle` on
the `PATH`. `python3` must be on the `PATH`; the tasks are run by
`editors/gradle/gradle_task.py`.

## Settings

`config` under `[language-server.kotlin-analyzer]` is sent as the server's
`initializationOptions`:

```toml
[language-server.kotlin-analyzer]
command = "kotlin-analyzer"
config = { diagnostics = { disabled = ["unused-import"], experimental = false }, gradleExport = true, projectJson = "/path/to/project.json" }
```

- `diagnostics.disabled`: checks that don't run. The checks are `syntax`,
  `unresolved-reference`, `missing-import`, `argument-mismatch`,
  `unsafe-call`, `non-exhaustive-when`, `unused-import` and
  `missing-package`.
- `diagnostics.experimental`: also run checks that are still experimental.
- `gradleExport`: export the Gradle model on start.
- `projectJson`: a model exported beforehand.

Inlay type hints need `display-inlay-hints = true` under `[editor.lsp]` in
`config.toml`.

Reload the workspace after a big change on disk:

- `:lsp-workspace-command kotlin-analyzer.reloadWorkspace` reloads the
  model and every file;
- `:lsp-restart` restarts the server.
