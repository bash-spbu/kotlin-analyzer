# Provenance

kotlin-analyzer is a new implementation. Its author, Alexander Bashkirov,
earlier worked on a private fork of kmp-lsp (MIT, by Juraj Oceliak) and wrote
parts of it himself. This file lists everything here that derives from that
earlier work, so that what is new and what was carried over stays clear.

Rules:
- Only work authored by Alexander Bashkirov may be carried over (verified with
  `git blame` in the fork). Nothing written by other kmp-lsp contributors is.
- Carried-over work is rewritten against this project's own APIs; the entry
  says what was taken (design, algorithm, tests) and from where.
- kmp-lsp is otherwise used only as a running binary, to compare behaviour.

## Carried over

| here | from (author's own work in the fork) | what was taken |
|---|---|---|
| — | — | nothing yet |

## Third-party code used as dependencies

| what | license | how |
|---|---|---|
| tree-sitter, tree-sitter-java, brokk-tree-sitter-kotlin | MIT | crates.io dependencies |
| `LICENSE` text | Apache-2.0 | the license's own text |
