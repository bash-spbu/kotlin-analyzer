# Command line

| command | does |
|---|---|
| `kotlin-analyzer` | the language server on stdin/stdout |
| `kotlin-analyzer analysis-stats <root>` | loads the workspace like the server and reports: files, modules, libraries, declarations; time per phase; peak memory; share of references and calls resolved exactly, and the reasons for the rest (`--only <reason>` prints examples); the share of values whose type is known — classes named as qualifiers and named arguments' labels aren't values — and the names, by kind, whose type isn't |
| `kotlin-analyzer diagnostics <root> [--project-json <file>] [--only <check,…>] [--experimental] [--limit <n>]` | every check over every workspace file; prints findings as `path:line:column [check] message` (the first `n`, 200 by default), per-check counts, p50/p95/max time per file and peak memory; exits 1 on any error or warning. `KOTLIN_ANALYZER_TRACE_MEMORY=1` prints RSS after each batch |
| `kotlin-analyzer parse <file>` | the syntax tree of one file, for debugging |
| `kotlin-analyzer inference <root> <project.json> <file>` | debugging: how long one file's inference takes and every name in its bodies left unresolved or with several candidates |
| `kotlin-analyzer definition <root> <project.json> <file> <line> <column>` | debugging: definition and hover at a zero-based position |
| `kotlin-analyzer --version`, `--help` | |

Environment: `KOTLIN_ANALYZER_LOG=<filter>` (e.g. `info`, `hir_ty=debug`)
logs to stderr; `KOTLIN_ANALYZER_LOG_FILE=<path>` logs to a file instead.
Caches live under `$XDG_CACHE_HOME/kotlin-analyzer` (default
`~/.cache/kotlin-analyzer`).
