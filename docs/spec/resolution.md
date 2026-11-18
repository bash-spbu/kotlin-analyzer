# Name resolution

Written from the Kotlin language specification (chapters *Declarations*,
*Scopes and identifiers*, *Overload resolution*, *Packages and imports*).

## What a source set sees

A name resolves only among declarations **visible to the source set** of the
file: its own source set, the source sets it depends on (see
project-model.md), and its libraries. A same-named declaration elsewhere in
the workspace is not a candidate. This is what makes duplicate generated
classes, other modules' copies and stale published versions harmless.

`internal` declarations are visible within their module only; `private`
top-level declarations within their file; `private` members within their
class.

## Classifiers (types, objects, `Foo.bar` qualifiers)

Levels, innermost first; the first level that has the name wins, and within a
level more than one candidate is an ambiguity (the answer is `Unknown`):

1. type parameters of enclosing declarations;
2. classifiers declared in enclosing blocks (local classes);
3. nested classifiers of enclosing classes and of their supertypes;
4. explicit imports (`import a.b.C`, `import a.b.C as D`);
5. the file's own package;
6. star imports (`import a.b.*`);
7. Kotlin's default imports: `kotlin.*`, `kotlin.annotation.*`,
   `kotlin.collections.*`, `kotlin.comparisons.*`, `kotlin.io.*`,
   `kotlin.ranges.*`, `kotlin.sequences.*`, `kotlin.text.*`;
8. the platform's default imports: `java.lang.*`, `kotlin.jvm.*` — so
   `String` is `kotlin.String`, not `java.lang.String`.

Among libraries, workspace sources come first, then the JDK, then the
source set's classpath in order: the first that declares a name has it.

A qualified name (`a.b.C.D`) resolves its first segment as a package prefix or
a classifier, then each following segment as a nested classifier.

## Calls

For `f(args)` without a receiver, the candidate levels are, innermost first:
local functions and local variables of function type; members and extensions
available through each implicit receiver (enclosing lambdas with receivers,
extension function receivers, enclosing classes and their companion objects);
then the import levels as for classifiers. For `r.f(args)`: members of `r`'s
type (including inherited), then extensions applicable to it at every level.

Overload resolution picks, at the innermost level with an *applicable*
candidate, the most specific one. Applicability considers: the number of
arguments (defaults, `vararg`, a trailing lambda), named arguments, and
argument types.

For diagnostics, a call is wrong only if **no candidate on any level** is
applicable — which requires every level's candidates to be known completely.

A call, navigation or index always binds tighter than a binary operator:
`a + b.f<T>()?.g` is `a + (b.f<T>()?.g)`, and `b.f` there is the call of
`f`, never a property `f` followed by an `invoke`. The parser sometimes
attaches such a chain to the whole operation; it is read as above for `+ - *
/ % .. ..<`, comparisons, `==`/`!=` and `?:`.

## Members of a type

Members of a class are its own declarations plus those of its supertypes, with
type arguments substituted. Kotlin sees some Java types through *mapped types*
(`java.util.List` ↔ `kotlin.collections.(Mutable)List`, `java.lang.String` ↔
`kotlin.String`, number boxes ↔ `kotlin.Int` etc.): their members are
Kotlin's view, not the Java class's. Members of `Any` (`equals`, `hashCode`,
`toString`) belong to every class.

## Local declarations

Within a body, a name binds to the innermost declaration in scope at that
point: parameters (function, constructor, lambda, catch), `val`/`var` declared
before the use in an enclosing block, `for` variables, destructured
components, `when (val x = …)` subjects, the implicit `it` of a lambda
without parameters. A later declaration in the same block does not bind an
earlier use. Smart casts narrow a stable value's type after a check
(`is`, `!= null`, `!!`), along the paths where the check holds, and as a
called function's contract says. The contract is the function's own —
read from its class file's Kotlin metadata or its written `contract { … }`,
never assumed from a name: after it returns (`returns()`: `requireNotNull`,
`checkNotNull`, `require`, `check`, kotlin.test's `assertNotNull`,
`assertIs<T>` with `T` written), where it gave `true` or `false`
(`returns(true)`/`returns(false)`: `!s.isNullOrEmpty()`), for the receiver
or any argument, positional or named, whether the call is a statement, an
initializer or an assigned value. `assert(x != null)` has no contract
(assertions may be off) and casts nothing. Not followed: `returnsNotNull()`,
a condition of two alternatives (`||`), a negated one, a type the call
infers rather than writes.

## Types in bodies

Every expression gets a type, or `Unknown`. A call's type parameters are
inferred from, in this order of evidence: explicit type arguments, the
receiver (`List<String>.map` binds `T`), the arguments (each inferred with its
parameter's type expected), the type the context expects (`val x:
List<String> = emptyList()`), and the lambdas' results (inferred last, their
parameters typed from the parameter they're passed to — a function type, or a
functional interface's single abstract method). Parameters nothing binds
become `Unknown`.

Among applicable overloads, the most specific wins: one whose parameter types
fit another's (a concrete type is more specific than a type parameter whose
bounds allow it); an extension's receiver counts as a parameter. Ties go to a
non-`vararg`, then to the one using fewer defaults, then to a non-generic one,
then — for Java's `f(int)` against `f(Integer)` — to non-null parameters. A
lambda fits a parameter only with the right number of parameters (`{ … }`
counts as none or one). A Java parameter of a mutable collection type also
takes the read-only collection. An argument breaking a callee type
parameter's bounds makes the overload inapplicable.

Java declarations have platform types (`String!`); Java getters and setters
read as properties (`user.name`, `user.active` for `isActive()`). The compiler
generates `copy` of data classes, `valueOf`, `values` and `entries` of enums,
and `componentN` for destructuring; the companions of superclasses are
implicit receivers in subclasses.

A declaration without a written type gets its body's type; one whose type
depends on itself is `Unknown`.

A class's primary-constructor parameters are visible in its initializers
(property initializers, `init` blocks, supertype arguments), not in its
functions and accessors — there the name is the property. Comments
are skipped wherever the first or next piece of code is looked for: a
body, a lambda or parentheses that start with a comment, an accessor on
its own line after one. `Name(…)` may
call an object's or a companion's `operator fun invoke` — a member, or an
extension on the companion in scope (`operator fun
SavedStateHandle.Companion.invoke(route)`) — and `Name { … }` the SAM
constructor of a `fun interface` or of a Java interface with one abstract
method (it takes that method as a function). An import of a Java class's
static member reaches the ones its superclasses declare (`import
ActivityCompat.checkSelfPermission` is `ContextCompat`'s). A member
property of function type is called before an extension function of its
name (`builder.test(scope, client)`, the receiver its first argument), and
an implicit receiver's property of extension function type can be called
on an explicit receiver (`origin.content()`). Member extensions apply
where both receivers are implicit (`with(template) { apply() }`). A
lambda's receiver hides the locals of the scopes around the lambda
(`channel` in `produce { channel.send(x) }` is the `ProducerScope`'s), not
the lambda's own parameters. A type parameter's bounds include its
`where` clause's. `x is Value` for a generic `Value` infers its type
arguments from `x`'s type (`ReferenceOr<Schema>` gives `Value<Schema>`).

In Kotlin Multiplatform code, where an `expect` and its `actual` are both
visible (platform code sees its common code) a name means the `actual`,
which takes its parameters' default values from the `expect`. An override takes
its parameters' default values from what it overrides. Member extensions of
the implicit receivers' classes are candidates for `receiver.f()`.

Compiled declarations deprecated as hidden (`@Deprecated(level = HIDDEN)`,
`@DeprecatedSinceKotlin(hiddenSince = …)`) don't exist for Kotlin code. A
Kotlin file facade belongs to the package its metadata names (`pn`), not
its class file's directory (`kotlin/time/jdk8/…` declares `kotlin.time`).

### Not modelled yet (stays `Unknown`, never reported)

- smart casts of properties (`a.b` after `a.b != null`), of a `var`
  after it's assigned a non-null value, and to `Nothing?` after `== null`;
- members of anonymous objects (`object { val x = 1 }`);
- declarations compiler plugins generate (`serializer()` of
  `@Serializable` classes);
- member extension properties, and member extensions on an implicit
  receiver;
- type parameters of local functions.
