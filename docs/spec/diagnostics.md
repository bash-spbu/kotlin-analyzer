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
| `unnecessary-not-null-assertion` | lint | `x!!` where `x` can't be null (below); offers "Remove `!!`" |
| `unnecessary-safe-call` | lint | `x?.f` where `x` can't be null; offers "Replace `?.` with `.`" |
| `useless-elvis` | lint | `x ?: y` where `x` can't be null; offers "Remove the elvis operator" |
| `senseless-comparison` | lint | `x == null`, `x != null` (or `===`, `!==`, `null` on either side) where `x` can't be null: always `false` or always `true` |
| `unused-variable` | hint (unnecessary) | a local `val`/`var` (one name, not destructured, not delegated, not `_`) never read: inference recorded no use and the name appears nowhere else in the enclosing declaration — not in a string template, not as a namesake |
| `deprecation` | lint | a use of a deprecated declaration (Kotlin's `DEPRECATION`): `'name' is deprecated. Message. Replace with 'x'.` — see "Deprecation" below |
| `deprecation-error` | error | the same for a deprecation of level `ERROR` (`DEPRECATION_ERROR`) |
| `can-be-val` | lint | a local `var` with an initializer that the enclosing declaration never assigns again — no `name = …`, `name += …`, `name++`, `--name` of any local of the name; offers "Change to `val`" |

A value *can't be null* when its type is a class or function type without
`?` — as declared, not through a smart cast: Kotlin doesn't smart-cast
every value our data flow narrows (a `var` a lambda writes), so a local
counts with the type it was declared or initialized with. A Java type
(`String!`, also through a generic: `Row.get(name, Class<T>): T` is `T!`),
a type parameter and anything unknown may be null. These are
Kotlin's own warnings, under the names `@Suppress` takes:
`UNNECESSARY_NOT_NULL_ASSERTION`, `UNNECESSARY_SAFE_CALL`, `USELESS_ELVIS`
and `SENSELESS_COMPARISON` come from the compiler (and fail an
`allWarningsAsErrors` build); `UNUSED_VARIABLE` (or `unused`) and
`CAN_BE_VAL` only from K2's extended checks, which IntelliJ runs and a
build doesn't. Where we're not sure we report less than Kotlin does, never
more.

### Compiler options

A check the Kotlin compiler reports itself (`UNNECESSARY_NOT_NULL_ASSERTION`,
`UNNECESSARY_SAFE_CALL`, `USELESS_ELVIS`, `SENSELESS_COMPARISON`,
`DEPRECATION`) follows the source set's compiler options (project-model.md):
an error where the build makes warnings errors (`allWarningsAsErrors`,
`-Werror`, `-Xwarning-level=NAME:error`), not reported where it suppresses
them (`-nowarn`, `-Xsuppress-warning=NAME`). IntelliJ-only checks
(`unused-variable`, `can-be-val`) don't.

### Deprecation

A declaration is deprecated by its `@Deprecated` — Kotlin's (with its
message, `ReplaceWith` and level), Java's, or a Javadoc `@deprecated` — and
a function or property also by a Kotlin `@Deprecated` on a member it
overrides (Java's doesn't pass to overrides); a constructor by its class's,
a type alias by its expansion's. In a Kotlin library it's read from
`@kotlin.Deprecated` alone: the compiler marks synthetic `$annotations`
methods and a deprecated property's accessors with the class file's
`Deprecated` attribute, which says nothing to Kotlin. Reported where the
declaration is used: a name in a body, a type (signatures, annotations,
supertypes), an import that imports nothing but deprecated declarations —
not an assignment's target, not a `name =` label. It stays silent:
- for a `HIDDEN` declaration — code can't reach one, so resolving to it is
  our mistake;
- for a function or constructor with an overload of another deprecation:
  overloads are chosen without every argument type judged, so the choice
  says nothing (`format("x")` beside a deprecated `format(String)`);
- for `@DeprecatedSinceKotlin` when the source set's Kotlin API version
  isn't known: its level is hidden, an error or a warning from the
  versions it names on (`warningSince = "1.5"`), none before;
- for the JDK's declarations when the model doesn't declare the release
  the code compiles against: the JDK the build runs on may deprecate what
  the module's toolchain doesn't (`URL(String)` in 20, for code built with
  JDK 8).

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
and `Result` may be `kotlin.Result`. In a file built with the Android SDK,
`R`, `BuildConfig` and `Manifest` aren't reported either: the Android Gradle
plugin generates them in the module's namespace before compiling. A file with syntax errors gets only
`syntax` diagnostics.

`@Suppress("…")` turns a check off where it applies — the file
(`@file:Suppress`), a declaration or an expression — under the name the
Kotlin tools give the same finding: `PackageDirectoryMismatch`
(`missing-package`), `UnusedImport` (`unused-import`), and the compiler's
names above.

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
positive by definition — except lints and hints, which code that
compiles may have: their findings are reviewed by hand against what
the compiler would say. `kotlin-analyzer diagnostics <root>` runs all checks
over every workspace file and exits non-zero on any finding; the gate is zero
findings on eight private Spring/Kotlin backend monorepos (260 to 8,400
files each). Known exceptions are
findings against generated code that is stale on disk (its source spec newer
than the generated file) — these are true.

Status (2026-11-17), stable checks:

| corpus | files | errors and warnings | which |
|---|---|---|---|
| backend A | 3677 | 14 | `argument-mismatch` against a stale generated client (one method, its 14 calls) |
| backend B | 8364 | 3 | `argument-mismatch` against a stale generated DTO; `missing-package` on a file that really has none; **false** `unsafe-call` after JUnit's `assertNotNull(x)`, whose contract (`x != null` once it returns) isn't read: only the `kotlin` package's contracts are known |
| backend C | 4416 | 0 | |
| backend D | 3819 | 0 | |
| backend E | 1332 | 1 | `missing-package` on a file that really has none |
| backend F | 1149 | 0 | |
| backend G | 631 | 0 | |
| backend H | 260 | 0 | |

(C to H weren't built: their generated sources aren't on disk, so names
from them stay unresolved — and the checks silent there.)

Mobile corpora (2026-11-14), open source, with the model the Gradle export
gives (Android SDK installed):

| corpus | files | types resolved | errors and warnings | which |
|---|---|---|---|---|
| android/nowinandroid (Android, Compose, Hilt) | 281 | 97.3% | 0 | |
| JetBrains/kotlinconf-app (Kotlin Multiplatform, Compose Multiplatform) | 260 | 92.9% | 0 | (28 `unused-import` hints, all true) |
| ktorio/ktor (Kotlin Multiplatform library) | 2344 | 90.6% | 5 | in tests: `filterIsInstance` on `KClass.annotations`, `receive` on a routing call, `called` assigned in a plugin's hook, a `Sink.write()` test helper, `maxAge?.toLong()` — not yet understood (15 `unused-import` hints, all true) |

Without the Gradle model (the directory layout guessed), ktor's sources in
`<target>/src` (`common/src`, `jvm/src`) aren't seen: 93 of 2457 files.

The pass checks files in batches of 128 and evicts cached results between
them: peak RSS ≈ 290 MB (backends A and C), ≈ 440 MB (backend B).
