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
7. default imports: `kotlin.*`, `kotlin.annotation.*`, `kotlin.collections.*`,
   `kotlin.comparisons.*`, `kotlin.io.*`, `kotlin.ranges.*`,
   `kotlin.sequences.*`, `kotlin.text.*`, `java.lang.*`, `kotlin.jvm.*`.

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
(`is`, `!= null`, `!!`, contracts like `requireNotNull`), along the paths
where the check holds.
