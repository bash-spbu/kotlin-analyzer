# Diagnostics

Every check is a renderer of facts the semantic model computed; a check
never resolves names on its own. A check reports only from exact facts —
whenever something it would need is `Unknown`, it stays silent.

| name | severity | reports |
|---|---|---|
| `syntax` | error | a syntax error (parse error or missing token) |
| `unresolved-reference` | error, **experimental** | a name that no visible declaration matches, when every level of the scope tower is known completely (see resolution.md) |
| `missing-import` | warning | an unresolved name for which importable declarations exist (a class, a function or property, or an extension the receiver can use); offers "Import" actions |
| `argument-mismatch` | error | a call no candidate accepts: wrong number of arguments, an unknown named argument, a missing argument without a default (argument types aren't judged) |
| `unsafe-call` | error | `x.f` / `x.f()` with a plain `.` where `x`'s type is nullable, `f` is a member of the class (not an extension, not `equals`/`hashCode`/`toString`), and nothing in the enclosing declaration could have smart-cast `x` (it's never compared with `null`, type-checked, asserted, assigned, or used with `?.`, `!!`, `?:`; `this.x` and `x` count as one) |
| `non-exhaustive-when` | error | a `when` over an enum, sealed type (an enum among its inheritors covered entry by entry) or `Boolean` that misses cases and has no `else` — unless a condition isn't modelled (ranges, `!is`, guards, arbitrary values) or the subject was compared, asserted (`requireNotNull`, `!!`, …) or assigned before it in the same declaration (Kotlin 2.x data flow may already exclude cases) |
| `unused-import` | hint (unnecessary) | an import whose name appears nowhere else in the file — not in code, not in comments — and isn't an operator or convention function (`plus`, `getValue`, `componentN`, `assign` of the assignment plugin Gradle's `kotlin-dsl` uses, …); offers "Remove" |
| `missing-package` | warning | a file that declares something but has no `package` line while its directory under the source root implies one; offers "Add package" |

### What "known completely" means

A lookup's miss is a fact only when everything it searched was modelled:
no implicit receiver of unknown type around, no lambda whose function type
is unknown (it may have a receiver), every class in the receivers'
hierarchies resolves all its supertypes, the file isn't a script, and the
name isn't one compiler plugins generate (`serializer`). A name an explicit
import names, or any name in a file with an unresolved star import, is never
reported as missing — the import is what's broken. Nor is any name when the
file can't see the packages default imports come from (`kotlin`,
`java.lang`): a model guessed from the directory layout knows no libraries,
and `Result` may be `kotlin.Result`. A file with syntax errors gets only
`syntax` diagnostics.

`@file:Suppress("…")` turns a check off for the file under the name the
Kotlin tools give the same finding: `PackageDirectoryMismatch`
(`missing-package`), `UnusedImport` (`unused-import`).

## Pipeline

- Computed per file on open, on change (debounced) and when the workspace
  finishes loading; a newer edit cancels an older computation, which then
  publishes nothing.
- Published with the document version it was computed for.
- Checks that need the whole workspace or libraries wait until loading has
  finished.
- Settings (`initializationOptions` or `workspace/didChangeConfiguration`,
  at the top or under `kotlin-analyzer`): `diagnostics.disabled` turns checks
  off by name, `diagnostics.experimental: true` turns experimental ones on.
- Before the workspace has loaded, only `syntax` runs.
- A check starts as *experimental* (off unless enabled) and becomes *stable*
  only after the corpus gate passes.

## Corpus gate

On projects that compile, every error or warning a check reports is a false
positive by definition. `kotlin-analyzer diagnostics <root>` runs all checks
over every workspace file and exits non-zero on any finding; the gate is zero
findings on `finops-mono`, `crypto-gateway-be`, `billing-mono`,
`payhost-mono`, `user-mono`, `jvm-common-tools`, `onewintoken-mono` and
`lottery-backend-mono`. Known exceptions are
findings against generated code that is stale on disk (its source spec newer
than the generated file) — these are true.

Status (2026-11-13), stable checks:

| corpus | files | errors and warnings | which |
|---|---|---|---|
| finops-mono | 3677 | 1 | `argument-mismatch` against a stale generated client |
| crypto-gateway-be | 8364 | 2 | `argument-mismatch` against a stale generated DTO; `missing-package` on a file that really has none |
| billing-mono | 4416 | 0 | |
| payhost-mono | 3819 | 0 | |
| user-mono | 1332 | 1 | `missing-package` on a file that really has none |
| jvm-common-tools | 1149 | 0 | |
| onewintoken-mono | 631 | 0 | |
| lottery-backend-mono | 260 | 0 | |

(The last six weren't built: their generated sources aren't on disk, so
names from them stay unresolved — and the checks silent there.)

Mobile corpora (2026-11-14), open source, with the model guessed from the
directory layout — no Android SDK here, so no Gradle export and no
libraries:

| corpus | files seen | errors and warnings | which |
|---|---|---|---|
| android/nowinandroid (Android, Compose, Hilt) | 309 of 310 | 0 | |
| JetBrains/kotlinconf-app (Kotlin Multiplatform, Compose Multiplatform) | 260 of 260 | 0 | (28 `unused-import` hints, all true) |
| ktorio/ktor (Kotlin Multiplatform library) | 93 of 2457 | 1 | `missing-package` on a build-logic file that really has none |

ktor keeps its sources in `<target>/src` (`common/src`, `jvm/src`), which
only its Gradle build knows — the layout guess sees its build logic alone.

The pass checks files in batches of 128 and evicts cached results between
them: peak RSS ≈ 290 MB (finops-mono, billing-mono), ≈ 440 MB
(crypto-gateway-be).
