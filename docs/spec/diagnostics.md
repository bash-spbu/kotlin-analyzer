# Diagnostics

Every check is a renderer of facts the semantic model computed; a check
never resolves names on its own. A check reports only from exact facts —
whenever something it would need is `Unknown`, it stays silent.

| name | severity | reports |
|---|---|---|
| `syntax` | error | a syntax error (parse error or missing token) |
| `unresolved-reference` | error | a name that no visible declaration matches, when every level of the scope tower is known completely (see resolution.md) |
| `missing-import` | warning | an unresolved name for which importable declarations exist; offers "Import" actions |
| `argument-mismatch` | error | a call no candidate accepts: wrong number of arguments, an unknown named argument, a missing argument without a default |
| `unsafe-call` | error | `x.f()` with a plain `.` where `x`'s type is nullable, the flow analysis says it may be null here, and `f` needs a non-null receiver |
| `non-exhaustive-when` | error | a `when` over an enum, sealed type or `Boolean` that misses cases, has no `else`, and is not narrowed by earlier checks of the same subject (Kotlin 2.x data-flow exhaustiveness) |
| `unused-import` | hint (unnecessary) | an import nothing in the file uses |
| `missing-package` | warning | a file that declares something but has no `package` line while its directory implies one |

## Pipeline

- Computed per file on open, on change (debounced) and when the workspace
  finishes loading; a newer edit cancels an older computation, which then
  publishes nothing.
- Published with the document version it was computed for.
- Checks that need the whole workspace or libraries wait until loading has
  finished.
- `diagnostics.disabled` in settings turns checks off by name.
- A check starts as *experimental* (off unless enabled) and becomes *stable*
  only after the corpus gate passes.

## Corpus gate

On projects that compile, every error or warning a check reports is a false
positive by definition. `kotlin-analyzer diagnostics <root>` runs all checks
over every workspace file and exits non-zero on any finding; the gate is zero
findings on `finops-mono` and `crypto-gateway-be`. Known exceptions are
findings against generated code that is stale on disk (its source spec newer
than the generated file) — these are true.
