# Agents (MCP)

`kotlin-analyzer mcp` is an MCP server (stdio, a JSON-RPC message per
line, protocol `2025-06-18`) giving a coding agent the analysis an editor
gets: tools `symbols`, `definition`, `references`, `hover`,
`implementations`, `outline`, `diagnostics`. A position is a file
(absolute, or relative to the process's working directory), a line from 1
and the symbol's name on it (a whole word), or a column from 1 in
characters. Answers are text: `path:line:column` — relative to the
project's root inside it — with the line, a declaration's kind and
container — a library's without sources marked `(no sources: declarations
rendered from the class files)`, its path the rendered file's (libraries.md,
Without sources); hover is markdown; a failure is a tool error saying why (a name
not on the line, a file outside any project, the project still loading).

## Hosts

The analysis is a *host*'s: a language server. Each listens for agents on
`<cache>/kotlin-analyzer/agent/<hash>.sock` (`<cache>` is
`$XDG_CACHE_HOME`, else `~/.cache`; the directory is its owner's only, the
socket mode 0600), `<hash>` a stable 64-bit FNV-1a of the canonical path of
each of its workspace folders. The line protocol: a request
`{"id": n, "query": {"kind": "definition", "at": {"file", "line", "symbol"
| "column"}}}` (or `"symbols"` with `query`, `"outline"`/`"diagnostics"`
with `file`), answered `{"id": n, "answer": "…"}` or `{"id": n, "error":
"…"}`. Queries only read; one asked before the workspace loads is
answered when it has. A host answers from a snapshot as it answers an
editor: an open document's unsaved text counts. When a live host listens
on a root already, another doesn't (it serves its editor only); a socket
nobody answers on is replaced. The socket goes when the server ends.
`agentSockets` in `initializationOptions`: `false` for none, or the
directory.

The MCP process finds the file's project — the nearest directory up with a
Gradle settings script, else with a build script or `.git`; none: an error,
so no host is started for a home directory — and asks its host: the
editor's language server when one listens, else one it starts in-process
(Gradle export in the background, as an editor's) that listens on the
socket too, so other agents' MCP processes share it. A git worktree has
its own settings script: it is a project of its own with a host of its
own, so agents in parallel worktrees each get theirs (one analysis per
worktree, in memory). Before each query to a host it started, the process
looks at the project's `.kt`, `.kts`, `.java` and `.gradle` files (hidden
directories, `build`, `out`, `target`, `node_modules` left out) and reports
what changed (`workspace/didChangeWatchedFiles`), so an agent's edits on
disk are seen. A host gone (its editor closed) is replaced once per query.
A query waits up to 45 s; past that the tool says the project is still
loading. On exit the process stops the hosts it started.

## Debugging

The same process debugs JVM code for the agent, its own session (one at a
time; not an editor's), over JDWP (crates `jdwp` and `debugger`):

| tool | does |
|---|---|
| `debug_start` | `test`: runs Gradle's test task for a `--tests` filter in the module holding `file`, with an init script that gives each `Test` (and `JavaExec`) JVM the debug agent through `jvmArgumentProviders` (a build's `jvmArgs = [...]` doesn't replace it), connecting to the debugger's free port and waiting for it (`server=n,suspend=y`; one fork); or `attach`: a JVM listening at `host:port`. Sets `breakpoints`, stops on `exceptions` (`uncaught` by default, `all`, `none`), runs, and waits for the first stop |
| `debug_breakpoint` | sets (or `remove`s) a breakpoint at a file's line: in the loaded classes of the file — its package, and the source file name each class records — and, through a class-prepare request, in those that load later. A file declaring inline functions also has its lines wherever they were inlined: any class whose source map names it (see Inline functions). With a `condition`, a `hitCondition` or a `logMessage` (see Breakpoint checks); `debug_start`'s breakpoints take them too |
| `debug_continue` | resumes and waits for the next stop, or keeps waiting while it runs |
| `debug_step` | `over`, `into` or `out` of a line; the JDK's and Kotlin's classes are stepped over, so is inlined code without a source in the project, and `over` steps over an inline function's body it didn't begin in; over a line where a suspend function suspends, the step ends where its coroutine resumes (see Coroutines) |
| `debug_pause`, `debug_stack` | suspends the program; the stopped thread's frames, numbered — an inline function's body `[inlined]` above its caller, a resumed coroutine's waiting functions `[suspended]` |
| `debug_variables` | a frame's (by `debug_stack`'s number) `this` and live locals — Kotlin's `$i$` markers left out, an inlined body's own locals (`$iv`) in its frame by their source names — or a suspended frame's saved state; with `expand` the fields or elements a path names |
| `debug_evaluate` | a path: a variable, `this` or its field, then `.field`, `[index]` (arrays, `ArrayList`), `.length`/`.size` of an array |
| `debug_output`, `debug_stop` | the program's and Gradle's latest output, logpoints' messages among it; ends the program |

A stop is told as text: why (a breakpoint, a step, a caught or uncaught
exception with its message), where (`path:line` and the line's source), the
project's frames (libraries' only counted) and the variables. Values show as
`42`, `"text"`, `int[3] {1, 2, 3}`, `Status.ACTIVE`, `ArrayList(size = 2)`,
a boxed value as its value; the JDK's value types (`java.math`, `java.time`,
`UUID`, `URI`) and Kotlin's data classes by their `toString()`, called in the
thread an event stopped — no other `toString()`, as it runs the program's
code; anything else as `Type@id`. Every stop suspends the whole JVM. A tool
waits up to `wait` seconds (50 by default, below a client's own timeout);
past that it says the program still runs.

### For editors

`kotlin-analyzer dap` is the same debugger for an editor (Zed's, VS Code's), over the
Debug Adapter Protocol on stdin/stdout. A configuration (`launch` or
`attach` arguments) is one of:

| configuration | debugs |
|---|---|
| `{ "request": "launch", "test": { "file", "filter" } }` or `{ …, "test": { "file", "package", "outerClass", "class", "method" } }` | the test, as `debug_start` does: Gradle's test task in the file's project, `--tests` the `filter` as given (VS Code's lenses) or of the pieces, `pkg.Outer$Class.method` (backticks dropped, empty pieces left out; Zed's runnables) |
| `{ "request": "launch", "program": "…/gradlew" \| "gradle", "args", "cwd", "env" }` | Gradle with `args` in the build at `cwd`: its `Test` and `JavaExec` JVMs |
| `{ "request": "launch", "program": "java", "args", "cwd", "env" }` | `java` with `args`, the agent first |
| `{ "request": "attach", "host", "port", "cwd" }` | a JVM listening for a debugger |

`launch` is answered once the JVM has connected, the program's output
streaming as `output` events meanwhile; then `initialized`, the editor's
breakpoints and exception filters (`uncaught`, on by default; `all`), and
`configurationDone` runs the JVM. Stops are `stopped` events (`breakpoint`,
`step`, `exception` with its message, `pause`); the end is `exited` with the
exit code, then `terminated`. Threads, frames and variable references are
small numbers, the last two good until the program runs again; a library's
frame has the source's name but no path, dimmed; an inlined body's frame is
named `function (inlined)`, a waiting coroutine's `function (Class,
suspended)`. `evaluate` takes the paths `debug_evaluate` does. Breakpoints
take a `condition`, a `hitCondition` and a `logMessage` (whose lines come as
`output`).

### Inline functions

Kotlin compiles an inline function's body into each caller, numbering its
lines past the end of the caller's file; the class's source map
(JSR-45 `SourceDebugExtension`, crate `debugger`'s `smap`) says where they
come from — its `Kotlin` stratum maps each line to a file and line, its
`KotlinDebug` stratum an inlined body's lines to the line of the call. A
frame in such a body is two: the body (`[inlined]`, its file and line,
named by Kotlin's `$i$f$<function>` marker) above the caller's (the call's
line). A lambda passed to an inline function keeps its own lines. A step
doesn't stop in code the project has no source of (`forEach`'s body), nor,
stepping over, in an inline function's body it didn't begin in. Not yet: a
step out of an inlined lambda leaves the whole method.

### Breakpoint checks

- `condition`: paths (as `debug_evaluate` takes them) and literals —
  numbers (`10L`, `1.5`), strings, chars, `true`, `false`, `null` —
  compared with `== != < <= > >=`, joined by `&& || !` and parentheses.
  Text, numbers and booleans compare by value (boxed ones too), other
  objects by identity. Evaluated in the innermost frame (an inlined body's
  when it is one); one that fails to evaluate stops, its error in the
  output.
- `hitCondition`: of the hits the condition lets through, `5` (the fifth),
  `== 5`, `> 5`, `>= 5`, `< 5`, `<= 5`, `% 5` (every fifth).
- `logMessage`: instead of stopping, a line in the output with each
  `{path}` filled in (text unquoted), `{{`/`}}` for braces.

Hits of breakpoints that don't stop let the program run on; a step ending
on one still stops as a step.

### Coroutines

A suspend function's state is a continuation (`BaseContinuationImpl`).
Resumed, it runs from wherever it was resumed, so the functions waiting
for it aren't on the thread: under its `invokeSuspend` frame the stack
shows them, `[suspended]`, following its `completion` chain and asking
each where it waits (`getStackTraceElement()`, from the compiler's
`@DebugMetadata`; called in the stopped thread). Their variables are the
continuation's fields (`L$0`, `I$0`, `label`…).

A step over a line where the function suspends: a method-exit request
watches the function return the suspension marker (`COROUTINE_SUSPENDED`);
the step then waits at its continuation's `invokeSuspend`, for that object
only, and from there goes on into the function, past its declaration's line
and the suspension's, to the next. A function returning from suspending
does so on its declaration's line, which a step doesn't stop on. Other
threads run meanwhile; a breakpoint elsewhere ends the step.

