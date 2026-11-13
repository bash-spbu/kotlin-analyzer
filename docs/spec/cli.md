# Command line

| command | does |
|---|---|
| `kotlin-analyzer` | the language server on stdin/stdout |
| `kotlin-analyzer analysis-stats <root>` | loads the workspace like the server and reports: files, modules, libraries, declarations; time per phase; peak memory; share of references and calls resolved exactly, and the reasons for the rest (`--only <reason>` prints examples) |
| `kotlin-analyzer diagnostics <root> [--only <check,…>]` | every check over every workspace file; prints findings as `path:line [check] message`, per-check counts and p95/max time per file; exits 1 on any error or warning |
| `kotlin-analyzer parse <file>` | the syntax tree of one file, for debugging |
| `kotlin-analyzer inference <root> <project.json> <file>` | debugging: how long one file's inference takes and every name in its bodies left unresolved or with several candidates |
| `kotlin-analyzer definition <root> <project.json> <file> <line> <column>` | debugging: definition and hover at a zero-based position |
| `kotlin-analyzer --version`, `--help` | |

Environment: `KOTLIN_ANALYZER_LOG=<filter>` (e.g. `info`, `hir_ty=debug`)
logs to stderr; `KOTLIN_ANALYZER_LOG_FILE=<path>` logs to a file instead.
Caches live under `$XDG_CACHE_HOME/kotlin-analyzer` (default
`~/.cache/kotlin-analyzer`).
