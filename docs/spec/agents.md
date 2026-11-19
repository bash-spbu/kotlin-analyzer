# Agents (MCP)

`kotlin-analyzer mcp` is an MCP server (stdio, a JSON-RPC message per
line, protocol `2025-06-18`) giving a coding agent the analysis an editor
gets: tools `symbols`, `definition`, `references`, `hover`,
`implementations`, `outline`, `diagnostics`. A position is a file
(absolute, or relative to the process's working directory), a line from 1
and the symbol's name on it (a whole word), or a column from 1 in
characters. Answers are text: `path:line:column` — relative to the
project's root inside it — with the line, a declaration's kind and
container; hover is markdown; a failure is a tool error saying why (a name
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
