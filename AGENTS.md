# AGENTS.md — working rules for kotlin-analyzer

## Provenance (read first)
- This project is written from scratch. Code is written from `docs/spec/`,
  the Kotlin language specification and rust-analyzer's architecture.
- **Never open, copy, translate or paraphrase source files of `kmp-lsp`**
  (`/Users/adm/Tools/kmp-lsp`). It may only be *run* as a binary, to compare
  behaviour on a corpus.
- Code carried over from earlier work of this project's author is listed in
  `PROVENANCE.md`, with where it came from. Add an entry when you carry
  something over; anything not listed there is new.
- Test fixtures are written fresh.

## Workflow
- `cargo xtask check` must pass before every commit: crate layering, `cargo
  fmt --check`, `cargo clippy -D warnings`, `cargo test`.
- Every behaviour change comes with a test that would have caught its absence.
  Tests include misleading cases: a same-named class in another module or
  package, overloads, generics.
- Corpus gate for diagnostics: zero error/warning findings on compiling code
  (`/Users/adm/Work/OneTech/finops-mono`, `/Users/adm/Work/OneTech/crypto-gateway-be`).
- Commit locally; the author decides when anything is pushed.

## Architecture rules
- Layers are crates; `xtask` keeps each crate's internal dependencies within
  its layer. Features (`ide`) ask `hir`; they never reach into `hir-def` or
  `hir-ty`.
- Every derived fact is a query of the database; read paths take `&self`.
- Analysis fails closed: when something can't be known exactly, the answer is
  `Unknown` and no diagnostic is reported.
- Nothing above `vfs` reads source text from disk.

## Code quality
- Types model behaviour: reach for an enum or newtype before a comment
  (`enum Resolution { Resolved(Def), Unknown(Reason) }`, not `Option` plus prose).
- Full-word names: `symbol`, `declaration`, `receiver` — never `sym`, `decl`,
  `rcv`, `s`, `c`.
- Explicit over clever: name compound conditions; prefer a plain loop to a
  long combinator chain.
- A function named `x_and_y` does two things — split it. A section comment
  inside a body marks a helper waiting to be extracted.
- Comments carry domain knowledge (why Kotlin behaves so), not narration.
