# kotlin-analyzer for VS Code

Kotlin support in one extension:

- **the Kotlin language**: `.kt` and `.kts` files get highlighting from this
  project's own TextMate grammar. They also get bracket matching,
  auto-closing, comment toggling, indentation and KDoc continuation.
- **the kotlin-analyzer language server**, for Kotlin and (read-only) Java:
  - definition into sources and libraries, hover, completion with
    auto-import, signature help;
  - references and rename;
  - semantic highlighting and inlay type hints;
  - document and workspace symbols, folding;
  - diagnostics with quick fixes.

  The server's semantic tokens refine the grammar's colours once the
  workspace has loaded.

Remove other Kotlin extensions that define the `kotlin` language
(`fwcd.kotlin`, `mathiasfrohlich.Kotlin`), or VS Code will pick one grammar
of the two.

## Install

Download `kotlin-analyzer-<version>.vsix` from the
[releases](https://github.com/bash-spbu/kotlin-analyzer/releases), then
run *Extensions: Install from VSIX…*, or:

```bash
code --install-extension kotlin-analyzer-1.0.1.vsix
```

The server binary is found in this order:

1. `kotlin-analyzer.server.path`, if set.
2. `kotlin-analyzer` on the `PATH`.
3. The release matching the extension's version, downloaded from GitHub on
   first use. Its SHA-256 is checked and it is kept in the extension's
   storage.

Releases exist for macOS and Linux on arm64 and x86_64. Elsewhere, build
the server with `cargo install --path crates/kotlin-analyzer` and set
`kotlin-analyzer.server.path`.

Open the Gradle build's root folder. The server exports the build's model
in the background, then reloads; until then it works from the directory
layout.

## Gradle tasks

*Tasks: Run Task → kotlin-analyzer* offers, for the active file's build:

| task | runs, in the build's root |
|---|---|
| `gradle build -x test` (build group) | build everything, without tests |
| `gradle clean` (clean group) | delete the build's outputs |
| `gradle test --rerun-tasks: :services:payments` (test group) | `gradlew -p services/payments test --rerun-tasks`: the project and its subprojects |

There is one test task for the file's project and one for each parent up to
the root (`:services`, `:`), for testing a group of projects. The project holding the file is its nearest ancestor directory with a
`build.gradle(.kts)`, and the build's root is the nearest one with a
`settings.gradle(.kts)`. Gradle is the root's `gradlew`, else `gradle` on
the `PATH`. `python3` must be on the `PATH`; the tasks are run by
`editors/gradle/gradle_task.py`.

In `tasks.json`:

```json
{ "type": "kotlin-gradle", "task": "test", "path": "${workspaceFolder}/services" }
```

## Settings

| setting | |
|---|---|
| `kotlin-analyzer.server.path` | the binary to run |
| `kotlin-analyzer.server.extraEnv` | extra environment, e.g. `JAVA_HOME` |
| `kotlin-analyzer.java.enable` | also serve Java files (turn off next to another Java server) |
| `kotlin-analyzer.diagnostics.disabled` | checks that don't run, e.g. `["unused-import"]` |
| `kotlin-analyzer.diagnostics.experimental` | also run experimental checks |
| `kotlin-analyzer.gradleExport` | export the Gradle model on start |
| `kotlin-analyzer.projectJson` | a model exported beforehand |
| `kotlin-analyzer.trace.server` | log LSP traffic to the output |

Changing the server, Java, Gradle or model settings restarts the server.
Diagnostics settings apply immediately.

Commands:

- *kotlin-analyzer: Reload Workspace* exports the model again if the build
  changed, then reloads every file.
- *Restart Server*.
- *Show Logs*.

Inlay hints follow VS Code's `editor.inlayHints.enabled`.

## Developing

```bash
npm ci
npm run check             # types, downloader unit tests, grammar tests
npm run test:integration  # a real VS Code: download, diagnostics, hover, definition
npm run package           # kotlin-analyzer-<version>.vsix
```

`test:integration` serves `target/release/kotlin-analyzer` as the release
(build it first). It runs the VS Code named by `VSCODE_EXECUTABLE`, or
downloads one. The grammar's tests are `test/grammar/*.kt`: each `^` line
asserts the scopes of the line above it.
