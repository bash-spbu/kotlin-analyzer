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
initializer, an assigned value or nested in another call's arguments
(`feed(requireNotNull(dog))`), and for an overloaded name whose every
candidate promises the same. `assert(x != null)` has no contract
(assertions may be off) and casts nothing. Not followed: `returnsNotNull()`,
a condition of two alternatives (`||`), a negated one, a type the call
infers rather than writes. A contract's `callsInPlace(block, EXACTLY_ONCE)`
(or `AT_LEAST_ONCE`) — `run`, `let`, `apply`, `also`, `with` — makes what
the lambda passed as `block` narrowed where its body ends hold after the
call (`run { requireNotNull(x) }; x.f()`); not after a safe call (`y?.let
{ … }` may not run it), nor when the lambda can leave early (`return@run`),
nor for overloads chosen among several. Such a lambda that always ends by
jumping (`apply { …; return x }`) makes the call's type `Nothing`: what
follows isn't reached.

Smart casts follow the data flow within a body:
- a loop (`while`, `do`-`while`, `for`) forgets on entering what was known
  of each `var` it assigns, as the compiler does (another iteration may
  come with another value); after a `while` or `do`-`while` without
  `break`, its condition is false (`while (x == null) x = next()`), and
  a `do`-`while`'s condition sees what its body narrowed and declared;
- a local `var` assigned a value has that value's type (when it fits the
  declared one) until assigned again;
- after an `if`, a `when` or a `try`, what holds is what every way out
  that doesn't jump (`return`, `throw`, `break`, `continue`, a call
  returning `Nothing`) knows: `if (x == null) x = Default`, `when { x ==
  null -> return; else -> … }`, `try { … } catch (…) { return }` (the
  try block's end, when every `catch` jumps). A `when` without `else`
  also has the way where no branch was taken;
- a branch of a `when` without a subject sees every earlier condition
  false; with a subject, a `null ->` branch tells later branches the
  subject isn't null, and a branch matching a value that can't be null
  (`when (x?.status) { Status.NEW -> … }`) sees the safe chain's root not
  null;
- `x?.f(…)` evaluates its arguments with `x` not null (`x?.let { x }`);
  `x?.y != null` and `x?.y == value` for a value that can't be null (of
  a non-null type, or of a type parameter bounded by one: `V : Any`) say
  `x` isn't null, and so does `x == value`; after `x ?: return` and
  `x?.y ?: return`, `x` isn't null;
- a `val` initialized by a condition narrows where it's used as one
  (`val ready = x != null; if (ready) x.f()`), a `var` in it until the
  `var` is assigned again;
- a `val` initialized by a safe chain (`val chain = info?.let { … }`)
  isn't null only when the chain's root (a `val` or parameter) isn't:
  narrowing the one narrows the other.

A stable property read from a stable value is narrowed as a local is
(`order.status` after `order.status != null`; `status` and `this.status`
are one), as the compiler's rules for stability say: a `val` with the
default getter, not delegated, `lateinit`, an extension or `expect`;
`private`, or final (not `open`, `abstract`, an interface's, or an
`override` in a class that can be extended) and of the same Gradle
module (whose `main` and `test` are friends). The value it's read from
is a `val`, a parameter, `this`, an object or such a property. A
property the compiler never smart-casts (a `var`, a getter, another
module's) keeps its declared type; an `open` one, or one read from a
`var`, is left as not followed. A library's member isn't trusted for
nullability: a Java class may override a Kotlin property's getter
(`getMessage()` of a Java exception for `Throwable.message`).

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
then — for Java's `f(int)` against `f(Integer)` — to non-null parameters.
A nearer class's member hides a farther one of the same JVM signature
only — classes compared by qualified name, a mutable collection as its
read-only one, an `expect` class not compared: a non-null `Long` is the
JVM's `long`, a nullable one, a Java
`Long` or a Java class's type argument a boxed `Long`, so AssertJ's
`isGreaterThan(long)` doesn't hide the inherited `isGreaterThan(ACTUAL)`
of `ACTUAL` = `Long`. A
lambda fits a parameter only with the right number of parameters (`{ … }`
counts as none or one). A Java parameter of a mutable collection type also
takes the read-only collection. An argument breaking a callee type
parameter's bounds makes the overload inapplicable.

A member whose result is its class's type parameter, which a star in the
type it's called on binds, gets that type when the parameter is the
class's own type (`SELF : AbstractStringAssert<SELF>`): AssertJ's
`assertThat(x).isEqualTo(y)` on `AbstractStringAssert<?>` stays an
`AbstractStringAssert<*>`, as the compiler's capture of the star
approximates it. Another bound isn't used: a star and a type that didn't
resolve are both unknown.

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

- smart casts of `open` properties on a value of a final type, of
  properties read from a `var`, to `Nothing?` after `== null`, and of a
  `var` captured by a lambda that changes it;
- members of anonymous objects (`object { val x = 1 }`);
- declarations compiler plugins generate (`serializer()` of
  `@Serializable` classes);
- member extension properties, and member extensions on an implicit
  receiver;
- type parameters of local functions.
