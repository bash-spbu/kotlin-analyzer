# Command line

| command | does |
|---|---|
| `kotlin-analyzer` | the language server on stdin/stdout |
| `kotlin-analyzer analysis-stats <root>` | loads the workspace like the server and reports: files, modules, libraries, declarations; time per phase; peak memory; share of references and calls resolved exactly, and the reasons for the rest (`--only <reason>` prints examples); the share of values whose type is known — classes named as qualifiers and named arguments' labels aren't values — how many of the rest are errors (a type naming nothing found), and the names, by kind, whose type isn't |
| `kotlin-analyzer diagnostics <root> [--project-json <file>] [--only <check,…>] [--experimental] [--limit <n>] [--memory]` | every check over every workspace file; prints findings as `path:line:column [check] message` (the first `n`, 200 by default), per-check counts, p50/p95/max time per file and peak memory; exits 1 on any error or warning. `KOTLIN_ANALYZER_TRACE_MEMORY=1` prints RSS after each batch. `--memory` prints, as the pass leaves the database (evicted between batches, as the server's pass), what it keeps by query and by kind of stored value: how many results or values, and Salsa's records of them (the records and their metadata, not what they point to); built with `--features memory-profile`, also each query's live bytes and blocks (allocated in it and not freed yet: its results, Salsa's records of them, whatever else outlived it) and all it allocated, and the same for what was allocated outside any query. That build counts each block for the query Salsa was running when it was allocated (a header per block; Salsa's span around each query says which), about a fifth slower than the plain one |
| `kotlin-analyzer mcp [--socket-directory <dir>]` | an MCP server on stdin/stdout for coding agents: definitions, references, hover, implementations, outlines, symbols and diagnostics, from the language server analysing the file's project when one runs, else from one it starts (see agents.md) |
| `kotlin-analyzer dap` | a Debug Adapter Protocol server on stdin/stdout for editors' debuggers (Zed's): JVM tests through Gradle, `java` and Gradle programs, or attaching to a JVM (see agents.md, Debugging) |
| `kotlin-analyzer parse <file>` | the syntax tree of one file, for debugging |
| `kotlin-analyzer log-whois <root> <hash>…` | which workspace file (or the workspace itself) a hash in a log stands for — on the machine that wrote the log (see logging.md) |
| `kotlin-analyzer anonymize <file>` | a Kotlin or Java file with its own names, strings and comments replaced, its syntax tree unchanged — to send with a report (see logging.md) |
| `kotlin-analyzer inference <root> <project.json> <file>` | debugging: how long one file's inference takes and every name in its bodies left unresolved or with several candidates |
| `kotlin-analyzer definition <root> <project.json> <file> <line> <column>` | debugging: definition and hover at a zero-based position |
| `kotlin-analyzer --version`, `--help` | |

Environment: `KOTLIN_ANALYZER_LOG=<filter>` (e.g. `info`, `hir_ty=debug`)
logs to stderr; `KOTLIN_ANALYZER_LOG_FILE=<path>` logs to a file instead.
Caches live under `$XDG_CACHE_HOME/kotlin-analyzer` (default
`~/.cache/kotlin-analyzer`).
