# Behaviour specification

What kotlin-analyzer does, as observable behaviour. Code is written from these
pages, the Kotlin language specification (<https://kotlinlang.org/spec/>) and
rust-analyzer's architecture — never from another implementation's source.

| page | covers |
|---|---|
| [project-model.md](project-model.md) | what a workspace is: modules, source sets, classpaths, generated code, the JDK |
| [resolution.md](resolution.md) | which declaration a name means, per the Kotlin spec |
| [libraries.md](libraries.md) | compiled JARs, sources JARs, the JDK |
| [jdk.md](jdk.md) | which JDK runs Gradle and is read, and downloading one when there is none |
| [profiling.md](profiling.md) | running tests or the application under async-profiler, and its flame graphs |
| [lsp.md](lsp.md) | the LSP requests served and what each answers |
| [diagnostics.md](diagnostics.md) | the checks, when they report, and the corpus gate |
| [cli.md](cli.md) | command-line tools |
| [logging.md](logging.md) | what the server logs, what it never does |

## Principles

1. **Exact or silent.** Every answer is either known exactly or `Unknown`. A
   diagnostic is reported only from exact facts; navigation may offer several
   candidates but never a guessed one.
2. **One semantic model.** Navigation, hover, completion and diagnostics read
   the same facts, so they never disagree about what a name means.
3. **Incremental.** An edit invalidates only what depends on it: changing a
   function body re-analyses that body — not the file's other bodies, nor the
   declarations of the module.
4. **Low memory.** Declarations are stored compactly with interned names
   and types; source text of libraries is read on demand; caches can be
   evicted, libraries' declarations too in the economical memory mode.
5. **No external tools.** No `rg`, `fd`, IDE exports or JVM: compiled classes
   are read in-process.
