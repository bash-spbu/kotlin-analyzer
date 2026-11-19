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

Also:

- completion items and workspace symbols are highlighted as Kotlin
  (`total(values: List<Money>): Long`, then where it's declared, dimmed);
- SQL is highlighted (with Zed's SQL extension) in a string that starts
  as a SQL statement does (`SELECT … FROM`, `WITH name AS (`, `INSERT
  INTO`, `UPDATE name SET`, `DELETE FROM`, `MERGE INTO`, in any case) —
  `jdbc.sql("""…""")`, `jdbcTemplate.query("…")` — in one after a
  `// language=SQL` comment, as IntelliJ marks it, and in Spring Data's
  `@Query("…")`; a string's pieces between templates (`$table`) are one SQL
  text. Regular expressions in `Regex("…")`, `"…".toRegex()` and
  `Pattern.compile("…")`, `TODO`/`FIXME` in comments
  (`languages/kotlin/injections.scm`);
- Enter inside a KDoc or block comment starts the line with `* `;
- quotes, backticks and `/*` don't close themselves inside strings and
  comments (`languages/kotlin/overrides.scm`).

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

## Agents

The extension adds an MCP server, `kotlin-analyzer` (*Settings → AI → MCP
Servers*), for the Agent Panel: Zed's agent and external agents (Claude
Code, Codex, … over ACP) get tools `symbols`, `definition`, `references`,
`hover`, `implementations`, `outline` and `diagnostics`. It runs
`kotlin-analyzer mcp` with the language server's binary, and answers from
the language server already analysing the project — the editor's view,
unsaved text included — through its socket; a project no language server
analyses (an agent's git worktree) gets one of its own, kept current with
the agent's edits on disk. Zed asks before each tool call unless
`agent.tool_permissions` allows `mcp:kotlin-analyzer:*`. The released
1.0.1 binary has no `mcp`: use a newer server (`binary.path`, or on the
`PATH`). `context_servers.kotlin-analyzer.command` sets another command.

## Running tests

Test functions (`@Test`, `@ParameterizedTest`, `@RepeatedTest`,
`@TestFactory`, `@TestTemplate`) get a ▶ in the gutter, and so do the
classes holding them, top-level or nested one level
(`languages/kotlin/runnables.scm`). It runs `gradle test --tests <filter>
--rerun` in the file's project — `app.OrderTest.creates an order`,
`app.OrderTest`, `app.Outer$Nested` — through `gradle_task.py zed-tests`.

The server's code lenses are off in Zed (`codeLens` defaults to
`{ "references": false, "tests": false }`): Zed doesn't run their commands,
and tests run from the gutter. Turning `references` on in `settings` shows
the reference counts with `"code_lens": "on"`, but clicking one does
nothing.

## Debugging

The extension adds Zed's debugger for Kotlin: the adapter `Kotlin` is the
server's `kotlin-analyzer dap` (the binary the language server runs, or
`dap.Kotlin.binary` in the settings), over JDWP — nothing else to install
but a JDK. A test's ▶ menu in the gutter offers *Debug* as well as *Run*
(the `kotlin-gradle` locator): the same `gradle test --tests …`, with the
debugger attached to the test JVM. Breakpoints, stepping, the call stack,
variables (expanding objects, arrays and lists), the debug console's
paths (`order.items[0].price`), and the values of names shown at the end of
their lines while stopped (`languages/kotlin/debugger.scm`).

Other sessions go in `.zed/debug.json` (schema
`debug_adapter_schemas/Kotlin.json`), or *debugger: start* → *Launch* with
a program `java` or `./gradlew`:

```json
[
  { "label": "bootRun", "adapter": "Kotlin", "request": "launch",
    "program": "$ZED_WORKTREE_ROOT/gradlew", "args": ["-p", "app", "bootRun"],
    "cwd": "$ZED_WORKTREE_ROOT", "env": { "SPRING_PROFILES_ACTIVE": "local" } },
  { "label": "attach :5005", "adapter": "Kotlin", "request": "attach", "port": 5005 }
]
```

A Gradle program's `Test` and `JavaExec` JVMs get the debugger (`bootRun`
is a `JavaExec`); attaching needs a JVM started with
`-agentlib:jdwp=transport=dt_socket,server=y,address=5005`. Attaching to a
process by its id isn't supported.

Breakpoints take a condition (`count > 3 && order.id == "A-1"` — paths and
literals, no calls), a hit count (`5`, `>= 5`, `% 10`) and a log message
(`total {order.total}`: a logpoint, which doesn't stop). An inline
function's body shows as a frame of its own above its caller, and its
lines take breakpoints wherever it was inlined; stepping goes through
inlined library code (`forEach`, `let`) without stopping in it. Under a
resumed coroutine the call stack shows the suspend functions waiting for
it, and stepping over a line where a function suspends stops at its next
line once the coroutine resumes. See `docs/spec/agents.md` (Debugging).

## Gradle tasks

With a Kotlin file open, *task: spawn* offers:

| task | runs, in the build's root |
|---|---|
| `gradle build -x test` | build everything, without tests |
| `gradle clean` | delete the build's outputs |
| `gradle test --rerun-tasks: this file's project` | `gradlew -p <project> test --rerun-tasks`: the file's project and its subprojects |
| `gradle test --rerun-tasks: choose the project or a parent` | lists the file's project and its parents up to the root (`:services:payments`, `:services`, `:`), and tests the one you pick |

To test a group of projects, run the test task from the parent's
`build.gradle.kts`, or pick the parent from the list. The project holding the file is its nearest ancestor directory with a
`build.gradle(.kts)`, and the build's root is the nearest one with a
`settings.gradle(.kts)`. Gradle is the root's `gradlew`, else `gradle` on
the `PATH`. `python3` must be on the `PATH`; the tasks are run by
`editors/gradle/gradle_task.py`.
Zed tasks can't reach a file inside an extension, so
`languages/kotlin/tasks.json` embeds the script, in base64. Each task is
one `/bin/sh -c` command quoted by `zed_tasks.py`, with no `args`: Zed puts
arguments into the shell's command line unquoted. The command isn't shown
(`show_command: false`); the script prints the Gradle command it runs. `editors/gradle/zed_tasks.py`
writes that file, and `editors/gradle/test_gradle_task.py` checks it.

## Settings

Under `lsp.kotlin-analyzer`:

- `binary.path` (and `arguments`, `env`): a binary instead of the one on
  the PATH or the download;
- `initialization_options`: `{ "diagnostics": { "disabled": ["unused-import"], "experimental": false }, "gradleExport": true, "projectJson": "/path/to/project.json" }`;
- `settings` (applied without a restart): `{ "diagnostics": { … }, "formatting": { "formatter": "ktfmt", "style": "google" } }`.

## Formatting

*editor: format* and `"format_on_save": "on"` run a formatter through the
server: [ktrs](https://github.com/Hexay/ktrs) by default — ktfmt's output
from a native binary, without a JVM — or
[ktfmt](https://github.com/facebook/ktfmt) itself, in Kotlin's coding
conventions:

```json
"lsp": {
  "kotlin-analyzer": {
    "settings": {
      "formatting": { "formatter": "ktfmt", "style": "kotlinlang" }
    }
  }
}
```

`formatter`: `ktrs` (the default), `ktfmt` or `off`; `style`: `kotlinlang`
(the default), `meta` or `google`; `path`: the formatter's executable or
ktfmt's `.jar`. Without `path`, the extension gives the server the
formatter on the worktree's `PATH`, else its pinned release (ktrs 0.5.3,
ktfmt 0.65), downloaded on first use into the extension's work directory
with its SHA-256 checked (`src/formatter.rs`); other versions' downloads
are removed. ktfmt has native builds for macOS arm64 and Linux x86_64;
elsewhere its jar is downloaded, which needs Java (`JAVA_HOME`, else `java`
on the `PATH`). These settings may also go in `initialization_options`.

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
