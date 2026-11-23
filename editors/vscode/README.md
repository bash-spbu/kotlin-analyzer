# kotlin-analyzer for VS Code

Kotlin support in one extension:

- **the Kotlin language**: `.kt` and `.kts` files get highlighting from this
  project's own TextMate grammar. They also get bracket matching,
  auto-closing, comment toggling, indentation and KDoc continuation. SQL
  in strings is highlighted with VS Code's SQL grammar: in a `"""` string
  from a line that starts as a SQL statement does (`SELECT`, `WITH name
  AS`, `INSERT INTO`, `UPDATE name SET`, `DELETE FROM`, `MERGE INTO`, in
  any case), in a `"` string that starts so (`SELECT … FROM` on its line),
  and in the string after a `// language=SQL` comment, as IntelliJ marks
  it. JSON and YAML (VS Code's grammars) in the `"""` string after a
  `// language=JSON` or `// language=YAML` comment; a `"` string stays a
  string, its `\"` escapes being no JSON. Templates (`$table`, `${id}`)
  stay Kotlin, inside a JSON value too.
- **the kotlin-analyzer language server**, for Kotlin and (read-only) Java:
  - definition into sources and libraries, hover, completion with
    auto-import, signature help;
  - references and rename;
  - semantic highlighting and inlay type hints;
  - document and workspace symbols, folding;
  - diagnostics with quick fixes.

  The server's semantic tokens refine the grammar's colours once the
  workspace has loaded.
- **the debugger**: the same binary, as `kotlin-analyzer dap`, for JVM
  tests and programs. Nothing to install but a JDK (see Debugging).

Remove other Kotlin extensions that define the `kotlin` language
(`fwcd.kotlin`, `mathiasfrohlich.Kotlin`), or VS Code will pick one grammar
of the two.

## Install

Download `kotlin-analyzer-<version>.vsix` from the
[releases](https://github.com/bash-spbu/kotlin-analyzer/releases), then
run *Extensions: Install from VSIX…*, or:

```bash
code --install-extension kotlin-analyzer-1.2.0.vsix
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

*Format Document* (and `editor.formatOnSave`) runs a formatter through the
server: [ktrs](https://github.com/Hexay/ktrs) by default — ktfmt's output
from a native binary, without a JVM — or
[ktfmt](https://github.com/facebook/ktfmt) itself
(`kotlin-analyzer.formatting.formatter`), in Kotlin's coding conventions
(`kotlinlang`) unless `kotlin-analyzer.formatting.style` says otherwise. The
formatter is `kotlin-analyzer.formatting.path`, else the one on the `PATH`,
else its pinned release (ktrs 0.5.3, ktfmt 0.65), downloaded on first use
with its SHA-256 checked and kept in the extension's storage. ktfmt has
native builds for macOS arm64 and Linux x86_64; elsewhere its jar is
downloaded, which needs Java (`JAVA_HOME`, else `java` on the `PATH`).

Code lenses show how many references each declaration has (click to peek
them), and *▶ Run test* / *▶ Run tests* above test functions (JUnit 4 and 5,
kotlin.test, TestNG) and the classes holding them. Running one starts the
`kotlin-gradle` task `gradle test --tests <filter> --rerun` in the file's
project (`"tests"` in `tasks.json` does the same); *Debug test* / *Debug
tests* next to each runs it under the debugger, *Profile test* / *Profile
tests* under async-profiler (see Profiling). *↑ Base.name* and *↓ N
implementations* over members and classes peek at what they override and
what implements them. *Expand Selection*
(`⌃⇧⌘→`) grows through the server's syntax-aware ranges.

Open the Gradle build's root folder. The server exports the build's model
in the background, then reloads; until then it works from the directory
layout.

## Profiling

*Profile test* over a test, or *kotlin-analyzer: Profile a Gradle Task (run,
bootRun, …)* for the application, asks what to profile — CPU, allocations,
locks, wall clock, or the first three at once — and which more
async-profiler options to add (`interval=1ms`, `alloc=512k`, `threads`,
`include=app.*`; the last answers are offered again). The run is a
`kotlin-profile` task: every test or `JavaExec` JVM Gradle starts gets
async-profiler's agent and writes a JFR recording when it exits (stop an
application by stopping the task). Each flame graph then opens in a tab of
its own; *Show the recordings* reveals
`build/kotlin-analyzer/profiles/<time>/`, where the recordings and pages
are.

async-profiler is `ASYNC_PROFILER_HOME`, `asprof` on the `PATH`, or its
latest GitHub release, downloaded once into
`~/.cache/kotlin-analyzer/async-profiler/` with its SHA-256 checked. A
keybinding can skip the questions: the command `kotlin-analyzer.profileTests`
takes the lens's test and `{ "event": "alloc", "options": "" }`. In
`tasks.json`:

```json
{ "type": "kotlin-profile", "path": "${file}", "event": "wall", "task": "bootRun" }
```

(`"tests": "app.OrderTest"` instead of `task` profiles tests); such a task's
flame graphs open in the browser.

## Debugging

The debug type `kotlin-analyzer` is the server's `kotlin-analyzer dap`,
over JDWP. Besides the *Debug test* lenses, `launch.json` takes:

```jsonc
{
  "configurations": [
    // A Gradle task's JVM: Test and JavaExec tasks (bootRun is one).
    { "type": "kotlin-analyzer", "request": "launch", "name": "bootRun",
      "program": "${workspaceFolder}/gradlew", "args": ["-p", "app", "bootRun"],
      "cwd": "${workspaceFolder}", "env": { "SPRING_PROFILES_ACTIVE": "local" } },
    // Tests, by Gradle's --tests filter.
    { "type": "kotlin-analyzer", "request": "launch", "name": "OrderTest",
      "test": { "file": "${file}", "filter": "shop.OrderTest" } },
    // `java` itself.
    { "type": "kotlin-analyzer", "request": "launch", "name": "Main",
      "program": "java", "args": ["-cp", "build/classes/kotlin/main", "shop.MainKt"] },
    // A JVM started with -agentlib:jdwp=transport=dt_socket,server=y,address=5005.
    { "type": "kotlin-analyzer", "request": "attach", "name": "attach", "port": 5005 }
  ]
}
```

Breakpoints in Kotlin and Java files take conditions (`count > 3 &&
order.id == "A-1"`: paths and literals, no calls), hit counts (`5`, `>= 5`,
`% 10`) and log messages (`total {order.total}`, a logpoint). The call
stack shows an inline function's body as a frame of its own, and under a
resumed coroutine the suspend functions waiting for it; stepping goes
through inlined library code and over a coroutine's suspension to its next
line. The debug console evaluates paths (`order.items[0].price`). See
`docs/spec/agents.md` (Debugging) for the details and what isn't there yet.

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
{ "type": "kotlin-gradle", "task": "test", "path": "${file}", "tests": "app.OrderTest" }
```

## Settings

| setting | |
|---|---|
| `kotlin-analyzer.server.path` | the binary to run |
| `kotlin-analyzer.server.extraEnv` | extra environment, e.g. `JAVA_HOME` |
| `kotlin-analyzer.java.enable` | also serve Java files (turn off next to another Java server) |
| `kotlin-analyzer.diagnostics.disabled` | checks that don't run, e.g. `["unused-import"]` |
| `kotlin-analyzer.diagnostics.experimental` | also run experimental checks |
| `kotlin-analyzer.formatting.formatter` | `ktrs` (default), `ktfmt` or `off` |
| `kotlin-analyzer.formatting.style` | `kotlinlang` (default), `meta` or `google` |
| `kotlin-analyzer.formatting.path` | the formatter's executable, or ktfmt's `.jar` |
| `kotlin-analyzer.codeLens.references` | reference counts above declarations |
| `kotlin-analyzer.codeLens.hierarchy` | *↑ Base.name* over overrides, *↓ N implementations* over what can be implemented |
| `kotlin-analyzer.codeLens.tests` | *▶ Run test* above tests and test classes |
| `kotlin-analyzer.gradleExport` | export the Gradle model on start |
| `kotlin-analyzer.projectJson` | a model exported beforehand |
| `kotlin-analyzer.trace.server` | log LSP traffic to the output |

Changing the server, Java, Gradle or model settings restarts the server.
Diagnostics, formatting and code lens settings apply immediately.

Commands:

- *kotlin-analyzer: Reload Workspace* exports the model again if the build
  changed, then reloads every file.
- *Invalidate Caches and Restart* deletes the project's caches (the exported
  model, its libraries' indexes and extracted sources) and reads the project
  as for the first time: Gradle exports it, every library is indexed anew.
- *Download a JDK* downloads one into `~/.jdks`, asking which vendor and
  release; without any JDK the server asks at startup.
- *Restart Server*.
- *Show Logs*.

Inlay hints follow VS Code's `editor.inlayHints.enabled`.

## Developing

```bash
npm ci
npm run check             # types, downloader unit tests, grammar tests
npm run test:integration  # a real VS Code: download, diagnostics, hover, definition, formatting, lenses, debugging
npm run package           # kotlin-analyzer-<version>.vsix
```

`test:integration` serves `target/release/kotlin-analyzer` as the release
(build it first). It runs the VS Code named by `VSCODE_EXECUTABLE`, or
downloads one; the debugging check needs `javac`. The grammar's tests are `test/grammar/*.kt`: each `^` line
asserts the scopes of the line above it.
