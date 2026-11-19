# The server's log

What the server writes about its own work, so a problem met on a codebase
we can't see can be traced from the log alone — and what it never writes.

## Never in a log

Source code, names (of declarations, locals, packages, modules, libraries),
string contents, comments, paths, the client's queries (a `workspace/symbol`
query, a rename's new name, a completion prefix), Gradle's output, a
formatter's output and messages. This is
held by the types events are built of, not by care at each call: a field can
only be a count, a number, a duration, a flag, a word of the server's own
source (a method, a kind, an outcome), a file reference, a keyed hash, a
short token from the client (its name and version: letters, digits, `._-+ `,
40 at most), or a panic's message scrubbed (what it quotes and anything
path-like cut out).

A file is `file#<id>:<hash>` — the server's id for it in the session and a
SipHash of its path relative to the workspace, keyed with a random key made
once per machine (`<cache>/kotlin-analyzer/log-key`, readable by its owner
only) and never logged. The
workspace is a hash of its root. `kotlin-analyzer log-whois <root> <hash>…`
tells the user, on their machine, which file a hash stands for.

## Levels

- **info** (default): the session's start (version, OS, architecture, CPUs,
  the client's name and version, the settings, the formatter), each workspace load (where
  the model came from; modules, source sets, Kotlin and Java files,
  libraries and those that failed; the time of each phase; memory), each
  Gradle export (the tool, how it ended, how long it took; on failure its
  kind and Gradle's exit code), settings changes (`formatting-changed`: the
  formatter; `code-lens-changed`: which lenses), a reload, the shutdown and
  the session's end (how it ended, requests received, panics).
- **warn**: a request slower than 1 s, diagnostics slower than 2 s — counting
  the time they waited for a free worker (`waited`) and the time they took
  (`took`), logged apart —, a load or an export that failed.
- **error**: a panic — its scrubbed message, where in the server's source it
  happened, what the thread was doing (the request: its id, method and file;
  the diagnostics of a file; a load; the export), and the stack's frames in
  the server's own code.
- **debug**: each request (id, method, file, line and column) and its answer
  (how it ended — ok, cancelled, error and its code —, how many items, how
  long it waited and took), each document opened, changed or closed (its size in bytes
  and lines, its version), each diagnostics run (the file, its version, how
  many found, how long), files changed on disk (how many), stale results
  dropped, each formatting (`formatted`: the formatter, the style, whether
  it ran from the `PATH`, an executable or a jar, how it ended —
  `formatted`, `not-found`, `not-started`, `timed-out`, `rejected` and the
  exit code, `not-utf8` —, how long, how many bytes the edit replaced), each agent's query
  (`agent-query`: which query, whether its file is the workspace's, answered
  or refused, how long).

The flight recorder keeps the last 1000 events below the level written; a
warning or an error writes them first (marked `recent`), so the steps that
led to it are in the log at any level.

A panic while answering a request is answered with an error (`InternalError`)
and the server goes on; one while checking a file leaves its diagnostics as
they were; one while loading or exporting ends the load (or export) as
failed. Writing the log never panics (a closed standard error is ignored),
and a panic while an event is being written isn't logged again.

## Where

- Standard error — the editor's server log — one line per event; the
  recorder's events only before an error.
- `<cache>/kotlin-analyzer/logs/<workspace hash>/<start>-<pid>.jsonl`, one
  JSON object per event (`t` seconds since the start, `seq`, `level`,
  `event`, `thread`, the fields); everything up to 5 MB per session, past
  that only warnings, errors and the recent events before them (up to 6 MB);
  the last 5 sessions kept. Its location is printed to standard error at the start.

The level: `log` in `initializationOptions` or in a configuration change
(`"info"`, `"debug"`), else `KOTLIN_ANALYZER_LOG`.

## When a log isn't enough

`kotlin-analyzer anonymize <file>` prints a Kotlin or Java file with its
own names, strings and comments replaced — the same name by the same
stand-in (`Type3`, `name7`, `"<s2>"`) — and the syntax tree unchanged;
names of the language, the JDK and common frameworks (and what's imported
from them) stay, so the file resolves as it did. The user reads it before
sending it.
