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
| `unsafe-call` | error | `x.f` / `x.f()` with a plain `.` where `x`'s type is nullable and `f` is a member of the class (not an extension, not `equals`/`hashCode`/`toString`). For a local or a stable property, after the smart casts the data flow gives it (resolution.md), and for a call's value or a property the compiler never smart-casts, the type decides, where the classpath is complete; for anything else (an `open` property, one read from a `var`, a library's), only when nothing in the enclosing declaration could have smart-cast `x` (it's never compared with `null`, type-checked, asserted, assigned, or used with `?.`, `!!`, `?:` — told once the body is inferred, by what each name there resolves to: `this.x`, `(x)` and `x` are one value, a local `x` shadowing the property another, a call's result none) |
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
| `opt-in-usage` | lint | a use of API that needs opt-in to a `WARNING` marker the code doesn't give (Kotlin's `OPT_IN_USAGE`) — see "Opt-in" below |
| `opt-in-usage-error` | error | the same for an `ERROR` marker, `@RequiresOptIn`'s default (`OPT_IN_USAGE_ERROR`) |
| `can-be-val` | lint | a local `var` with an initializer that the enclosing declaration never assigns again — no `name = …`, `name += …`, `name++`, `--name` of any local of the name; offers "Change to `val`" |
| `useless-cast` | lint | `x as T` where `x`'s type is exactly `T` (`as?` of a nullable `x`: `T?`), `T` not a type parameter (after `require(this is T)` the compiler sees `X & T`) — Kotlin 2.3 on (`USELESS_CAST`); offers "Remove the cast" — see "Casts and type checks" below |
| `cast-never-succeeds` | lint | `x as T` where no value of `x`'s type can be a `T` — Kotlin 2.3 on (`CAST_NEVER_SUCCEEDS`) |
| `unchecked-cast` | lint | `x as T` the run time can't check: to a type parameter that isn't reified, or to type arguments `x`'s type doesn't determine (`UNCHECKED_CAST`) |
| `useless-is-check` | lint | `x is T` that is always `true` (or `x !is T` always `false`): `x`'s type is a `T` (`USELESS_IS_CHECK`) |
| `redundant-else-in-when` | lint | an `else` in a `when` whose other branches cover every case of the subject's enum, sealed type or `Boolean` (`null` too, for a nullable or Java subject) — under the same conditions as `non-exhaustive-when`, and only for a sealed type whose source set no other of its module refines (`REDUNDANT_ELSE_IN_WHEN`); offers "Remove the `else` branch" |
| `return-type-mismatch` | error | a returned value (`return x`, an expression body) whose class can't be the one the function declares (Kotlin's `RETURN_TYPE_MISMATCH`) — see "Type mismatches" below |
| `type-mismatch` | error | the same for a declared property's or local's initializer, a parameter's default value, an assignment to a `var` that writes its type, and an argument of the only function a call can mean (`INITIALIZER_TYPE_MISMATCH`, `ASSIGNMENT_TYPE_MISMATCH`, `ARGUMENT_TYPE_MISMATCH`) |
| `duplicate-branch-condition` | lint | a `when` condition an earlier branch already checks: the same literal, enum entry or object, or `is`/`!is` of the same type; branches with a guard don't count (`DUPLICATE_BRANCH_CONDITION_IN_WHEN`) |
| `val-reassignment` | error | `x = …`, `x++`, `x--` where `x` is a local `val` that already has a value (a `val` declared without one may get its first), a parameter, a loop's or a lambda's variable (`VAL_REASSIGNMENT`); `x += …` isn't judged (`plusAssign` may take it) |
| `missing-return` | error | the closing brace of a block-bodied function whose result type isn't `Unit` when its end is reached: no statement jumps (`return`, `throw`, a call returning `Nothing`, `while (true)` without `break`, an in-place lambda that always does — `apply { return x }`) and every statement's type is known (`NO_RETURN_IN_FUNCTION_WITH_BLOCK_BODY`) |
| `nothing-to-override` | error | `override` on a member no supertype has one of the same name, kind and JVM signature (`NOTHING_TO_OVERRIDE`) — see "Overrides" below |
| `missing-override` | error | a member with an inherited, non-private member's name, kind and JVM signature that doesn't say `override` (`VIRTUAL_MEMBER_HIDDEN`) |
| `abstract-member-not-implemented` | error | a class or object, not abstract or sealed, that implements nowhere in its hierarchy an abstract member it inherits (`ABSTRACT_MEMBER_NOT_IMPLEMENTED`, `ABSTRACT_CLASS_MEMBER_NOT_IMPLEMENTED`); reported on the class's name, with the first such member |

A value *can't be null* when its type is a class or function type without
`?`: after smart casts for a value that can't change (a `val`, a
parameter, a stable property — resolution.md), as declared for a `var`
(Kotlin doesn't smart-cast every `var` our data flow narrows: one a
lambda writes); a call's value only when its type is the compiler's for
certain (not a generic call's: a use-site projection this model drops,
`MutableList<in Int>.first()`, would make it look non-null). `x!!`,
`x?.f` and `x ?: y` are the compiler's data-flow
warnings, reported from language version 2.3 on (`EnableDfaWarningsInK2`)
and not below it or when the version is unknown; a comparison with
`null` it reports at every version. A Java type
(`String!`, also through a generic: `Row.get(name, Class<T>): T` is `T!`),
a type parameter and anything unknown may be null. These are
Kotlin's own warnings, under the names `@Suppress` takes:
`UNNECESSARY_NOT_NULL_ASSERTION`, `UNNECESSARY_SAFE_CALL`, `USELESS_ELVIS`
and `SENSELESS_COMPARISON` come from the compiler (and fail an
`allWarningsAsErrors` build); `UNUSED_VARIABLE` (or `unused`) and
`CAN_BE_VAL` only from K2's extended checks, which IntelliJ runs and a
build doesn't. Where we're not sure we report less than Kotlin does, never
more.

### Overrides

`nothing-to-override`, `missing-override` and
`abstract-member-not-implemented` compare members as the JVM does: by
name, kind (function or property) and erased parameter types — a class
by its qualified name (two jars may hold one class), a non-null `Int`
as the primitive and a nullable one, a Java `Integer` or a type argument
as the boxed class, `MutableList` as `List`, a type parameter as
substituted from the class checked. They run only on JVM code with the
whole classpath, for a class whose whole hierarchy resolves and has no
`expect` or `actual` class in it (the `expect` may lack what the
`actual` declares), and a member whose types all resolve. Silent: an
`override` that may override a supertype's member whose types don't all
resolve, or a Java accessor (`override val size` of a class extending
`java.util.AbstractMap`, whose `size()` Kotlin reads as the property),
or a member of a JDK class a Kotlin built-in in the hierarchy maps to
(`forEach(Consumer)` of `java.lang.Iterable`); a class implementing a
supertype `by` a delegate; an abstract member of a built-in collection
(a Java collection class implements `MutableMap.remove(K)` as
`remove(Object)`). An abstract Java method without parameters counts as
implemented by a property of its name or its getter's, and an abstract
property by a Java getter (`getSavedStateRegistry()`).

### Compiler options

A check the Kotlin compiler reports itself (`UNNECESSARY_NOT_NULL_ASSERTION`,
`UNNECESSARY_SAFE_CALL`, `USELESS_ELVIS`, `SENSELESS_COMPARISON`,
`DEPRECATION`, `OPT_IN_USAGE`, `USELESS_CAST`, `CAST_NEVER_SUCCEEDS`,
`UNCHECKED_CAST`, `USELESS_IS_CHECK`, `REDUNDANT_ELSE_IN_WHEN`,
`DUPLICATE_BRANCH_CONDITION_IN_WHEN`) follows the source set's compiler options (project-model.md):
an error where the build makes warnings errors (`allWarningsAsErrors`,
`-Werror`, `-Xwarning-level=NAME:error`), not reported where it suppresses
them (`-nowarn`, `-Xsuppress-warning=NAME`). IntelliJ-only checks
(`unused-variable`, `can-be-val`) don't.

### Type mismatches

Like rust-analyzer's, inference records a mismatch in one place, where a
type is required rather than hinted — a requirement apart from the
expected type that guides inference (`Expectation::HasType` there). A
requirement on a branching value passes to the value each branch ends
with, as rust-analyzer's coercion of each branch: `if`/`else`, every
`when` branch, `try` and each `catch`, the right side of `?:`, the last
statement of a block, through parentheses — `fun f(): Dog = if (c) dog
else car` is reported on `car`. The range shown is the salient part: the
member name of a call or property chain (`getOrThrow` of
`client.convert(…).getOrThrow()`), not the whole expression. Reported
only when the value's class can't be the required one — no supertype of
it is, every supertype known; a mutable collection required takes its
read-only one (a Java `List` parameter is `(Mutable)List!`), and a class
of the same qualified name counts as the same. Nullability alone is
reported for a local or a stable property, after the smart casts the
data flow gives it (resolution.md), and for a call's value or a property
the compiler never smart-casts (`feed(repo.find())`, `feed(holder.dog)`
of a `var dog`), only where the source set's classpath is complete — a
missing library's contract would have narrowed a local. Silent:
- for nullability alone of anything else (an `open` property, one read
  from a `var`, a library's); a nullable name going where a nullable
  value goes may be smart-cast to `Nothing?` (`if (x == null) f(x)`);
- for type arguments, but those of a type parameter declared `out` or
  `in` (`List<out E>`: `List<Car>` where a `List<Dog>` goes; `Map<K, out
  V>`; `Comparable<in T>`): a use-site projection (`MutableList<out
  Animal>`) isn't kept, and on an invariant parameter it would change the
  answer; and not of a call's argument (a candidate whose parameter's
  type arguments don't fit makes the compiler look further out);
- for a value whose type isn't the compiler's for certain (also what
  cast checks require, below):
  - anything but a name, property, `this`, literal or call of a function
    without type parameters — a generic call may take its type from where
    it goes; lambdas and references (a lambda's result isn't required
    yet); the left side of `?:`;
  - a local whose type is neither written nor that of such a value
    (`val charset = a ?: b`: a common supertype isn't followed);
  - a name both a local (visible or not) and a receiver's member have —
    which one wins varies with nesting (an object expression in an
    initializer sees the constructor's parameter); a constructor's `val`
    parameter is its property, not two names;
  - a call or a receiver's member inside a lambda whose receiver isn't
    known (`build<T> { … }`: `T` may have members of any name), or a
    member read through a value that isn't certain itself;
  - a call chosen among several applicable overloads, or a function
    chosen where a property of function type of its name could be
    invoked instead;
  - a Java value (`T!`), whose type may come through generics not
    followed; numbers (an integer literal converts); `expect` classes and
    calls of `expect`/`actual` members (an `actual typealias`, an
    extension of another platform);
- for a `return` inside a lambda (an inline lambda's leaves the function
  around it) or labelled;
- for an assignment to a `var` whose type is inferred;
- for an argument when other functions of the name exist, for `vararg`
  and spread arguments, and for parameters whose type depends on the call.

Inference follows Kotlin for what these depend on: a `private` function
of another file or of a library isn't called (an extension of its name
is); a property several unrelated supertypes declare has the most
specific type (an intersection override: `IrSimpleFunctionSymbol.owner`
is an `IrSimpleFunction`); a call with type arguments only considers
functions taking as many; a `vararg Int` parameter is an `IntArray`.

### Casts and type checks

Kotlin's rules, from its cast checker: a cast is useless when the value's
type equals the target; it never succeeds when the two types are unrelated
(neither a subtype of the other) and one is a final class, or both are
classes, not interfaces; it's unchecked when it targets a type parameter
that isn't reified (unless the value is that parameter already), or type
arguments the value's type doesn't fix: `Any as List<String>`, but not
`Map<K, V> as MutableMap<K, V>` nor `x as List<*>`. A type check is
useless when the value's type is a subtype of the type checked — not on
the last branch of a `when` without `else`, where the compiler accepts it.
The value's type is the one inference gives, smart casts included, as the
compiler judges by the smart-cast type too. Silent:
- for a value that isn't a name, property, `this`, plain string or a call
  of a function without type parameters (a generic call may take its type
  from the cast itself, and the compiler then doesn't warn);
- when either type has an unknown part, a function type, or Java types
  nested in its arguments (`List<String!>`);
- `cast-never-succeeds` for generic classes, two nullable types and two
  numbers (they have warnings of their own), `java.*` classes (mapped to
  Kotlin's), enums, `expect` classes, and a workspace class with an
  annotation (`kotlin-spring` and other `allopen` plugins open those);
- `useless-cast` and `cast-never-succeeds` before Kotlin 2.3, or when the
  language version isn't known: they're data-flow warnings the compiler
  enables from 2.3 on;
- where the grammar splits the type (`(x is A).B` for `x is A.B` at the
  end of a file).

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

### Opt-in

A marker is an annotation class with `@RequiresOptIn(message, level)`. A
declaration needs the markers it's annotated with; a class also its outer
classes', a constructor its class's, a member its class's (the receiver's
type). Uses are names in bodies and in types — not an annotation's own
class (`@Beta` is how one opts in), not a `name =` label. A use is accepted
when the module opts in (`optIn`, `-opt-in`, `languageSettings.optIn`), or
an enclosing declaration, annotated expression or the file is annotated
`@OptIn(Marker::class)` or with the marker itself. Reported with the
marker's message when it has one. Silent:
- when the source set's compiler options aren't declared (a model
  exported before they were, or guessed): what the module opts into isn't
  known;
- for a function or constructor whose overloads need other markers;
- for markers on the classes of a callable's signature,
  `@SubclassOptInRequired` and overrides, which aren't modelled.

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
| backend B | 8364 | 3 | `argument-mismatch` against a stale generated DTO; `missing-package` on a file that really has none; a `unsafe-call` after JUnit's `assertNotNull(x)` was false while only the `kotlin` package's contracts were known; with contracts read from metadata it's gone |
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
