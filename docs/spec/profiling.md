# Profiling

Running a test, a test class or the application under
[async-profiler](https://github.com/async-profiler/async-profiler) and
showing its flame graph. It is a task of `editors/gradle/gradle_task.py`, the
script every editor runs Gradle tasks with; nothing of it is in the server.

## What runs

| task | in Zed | Gradle |
|---|---|---|
| `profile-tests <path> <filter>` | the gutter's ▶ menu on a test function or class: *profile: gradle test --tests …* (`zed-profile-tests`) | `test --tests <filter> --rerun` in the project holding the file |
| `profile-run <path>` | the task *profile: a Gradle task of this file's project* | the task asked for (Enter: `run`; `bootRun` for Spring Boot) of the project holding the file |

Before Gradle runs, the task asks in the terminal, each answer's default the
last one given (Enter keeps it):

1. **What to profile** — `cpu` (CPU time, the first default), `alloc`
   (allocated memory), `lock` (lock contention), `wall` (wall-clock time of
   every thread, sleeping and waiting too), or `all` (`cpu`, `alloc` and
   `lock` in one run).
2. **More async-profiler options** — agent options added after the event,
   comma-separated as async-profiler takes them (`interval=1ms`,
   `alloc=512k`, `threads`, `include=app.*`); `-` for none. The output file
   is the task's: a `file=` among them is refused.

Without a terminal to ask in, the last answers (else `cpu`, no options) are
used. The answers are kept in the cache directory (`profiler.json`).

Every `Test` and `JavaExec` JVM of the build gets the agent
(`-agentpath:<libasyncProfiler>=start,<event>,<options>,file=…/profile-%p.jfr`,
by an init script, through `jvmArgumentProviders`) and writes a JFR
recording when it exits — one per JVM. An application runs until it stops by
itself or is stopped (Ctrl-C in the terminal: Gradle stops its JVM, which
writes its recording); the task waits for Gradle either way.

## The result

The recordings go to `<build root>/build/kotlin-analyzer/profiles/<time>/`
with, for each event recorded, a flame graph of all of them together
(`cpu.html`, `alloc.html`, `lock.html`, `wall.html`) made by async-profiler's
`jfrconv`, titled with what was profiled. The task prints their paths and
opens them: with `KOTLIN_ANALYZER_OPEN_WITH` when set, else `open` (macOS) or
`xdg-open` (Linux) — the default browser, as an editor can't show the page
itself (Zed has no HTML view). No recording (no JVM ran: the tests were
filtered out, the build failed first) is said so, and Gradle's exit code is
the task's.

## Which async-profiler

The first of: `ASYNC_PROFILER_HOME` (a directory with
`lib/libasyncProfiler.dylib` or `.so`); `asprof` on `PATH`, its directory's
parent (Homebrew's); the newest one downloaded before. With none, the
latest release is downloaded from GitHub
(`api.github.com/repos/async-profiler/async-profiler/releases/latest`): the
asset for this platform (`macos.zip`, `linux-x64.tar.gz`,
`linux-arm64.tar.gz`, not `-debug`), with `curl` over HTTPS, its SHA-256
checked against the release's `digest` — an asset without one isn't used —
and unpacked (no entry outside its directory) into
`<cache>/async-profiler/<release>/`. Nothing is left behind when a step
fails.

## The JDK

Gradle and `jfrconv` need Java. When `JAVA_HOME` holds no JDK and `java` on
`PATH` doesn't run one (macOS's stub with no JDK installed), every task of
the script runs them with `JAVA_HOME` set to the newest JDK in `~/.jdks`
(see jdk.md), when there is one.
