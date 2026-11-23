# Libraries

A library is one classpath entry of a source set: a compiled JAR, optionally
with a sources JAR, or the JDK.

## What is read

| source | read how | gives |
|---|---|---|
| compiled JAR | its class files, in-process: Java classes from their constant pools and generic signatures, Kotlin classes and top-level declarations from `@kotlin.Metadata`, Kotlin's built-in types from `.kotlin_builtins` | declarations with exact signatures: fully qualified types, nullability, defaults, `vararg`, extension receivers, `suspend` |
| JDK | `lib/ct.sym`, the `.sig` class files of the project's release (`P` for 25) | the JDK's API, as declarations |
| sources JAR / JDK `src.zip` | one entry at a time, parsed like a workspace file | where a declaration is written (navigation) and its documentation (hover) |

Each archive is indexed once at load: per package, its top-level names and
which entries hold its declarations. A reload (the model exported again, a
library added) indexes again only the archives whose modification time or
length changed since they were indexed, and new ones: the others keep their
index, and what was read and inferred from them stays. Indexes are kept
on disk too, in `$XDG_CACHE_HOME/kotlin-analyzer/library-index/` (one file
per archive, named by a hash of its path): a load — the first after a
restart included — takes an archive's index from there when it was made
for the same archive, kind and sources, with the same modification time
and length, by the same version of the server; else it indexes the archive
and writes the file (written whole or not at all). A file that can't be
read is ignored. A package's
declarations are read when a query first needs them and kept for the
session. Once read from the archive, they are kept on disk too, in `$XDG_CACHE_HOME/kotlin-analyzer/library-trees/`
(a directory per library, named by its archive and a hash of the archive,
kind and sources; a file per package): reading them again after a
restart takes them from there
when they were read from the same library with the same modification time
and length, by the same version of the server; else from the archive,
writing the file (whole or not at all). Nothing runs on a JVM.

## Which library a name comes from

Workspace sources first (see resolution.md); then the JDK; then the source
set's classpath in the build's order — the first library that declares a
name has it, as for the compilers. A source set sees only its own classpath:
a dependency module's `implementation` libraries aren't on it.

## Navigation into libraries

The declaration's source is found in the sources archive by the file its
class was compiled from (the `SourceFile` attribute), or else by searching
its package's directory (under any source set's root, `commonMain/`,
`jvmMain/`, a JVM one first), then by its chain of names (class, nested
class, member). Kotlin's built-in classes, which no class file declares,
are declared in files named otherwise (`kotlin.Int` in `Primitives.kt`),
some outside their package's directory (`kotlin.collections.List` in
`kotlin/Collections.kt`): last, the files of the package's directory and
of the ones enclosing it that declare the package are searched. The entry is written to
`$XDG_CACHE_HOME/kotlin-analyzer/library-sources/<archive>-<hash>/<entry>`
for the editor to open.

### Without sources

A library without a sources archive — or whose archive has no file
declaring what is asked for — opens as its declarations rendered from the
class files (`hir-def`'s `decompiled`), as an IDE shows a compiled class:
a Kotlin class (with `@Metadata`) as Kotlin, a Java class as Java; the
other declarations compiled from the same source file together (a Kotlin
file's top-level functions); members as constructors, properties,
functions, nested classes; signatures, modifiers, supertypes and enum
entries as recorded, types by their simple names with imports, Kotlin
parameters with defaults `/* = … */`. A Java class's method bodies (and its
static initializer, `static { … }`) are decompiled where they run straight
through (`classfile`'s `decompile`, below). A Kotlin file keeps its
declarations in Kotlin and shows each function's and constructor's body as
Java, decompiled the same way, in `//` lines under a `// Decompiled to
Java:` line inside the Kotlin block — no Kotlin decompiler, the bytecode
read as what it is. A Kotlin method is found by the class its metadata
names for the code (a class's own, a file's `…Kt` facade, or a multi-file
facade's part, `CollectionsKt__CollectionsKt`, never the facade's
delegating stub), an extension's receiver as its first parameter, Kotlin's
`Int`/`Int?`/`IntArray` as `int`/`Integer`/`int[]` to tell overloads apart;
the compiler's leading `Intrinsics.checkNotNull…` calls are left out. A
`suspend` function's method takes a `Continuation` more and is a state
machine: it matches nothing. Every other body is
`{ /* compiled code */ }`. Nothing runs: no JVM — a reading aid for a
library's API, not its source.

#### Decompiled bodies

A body's bytecode is cut into basic blocks; each replays on its operand
stack with expressions in place of values: `aload_0; getfield name;
areturn` is `return this.name;`. Read: constants, locals (named and typed
by `LocalVariableTable`, generic types by `LocalVariableTypeTable`; else
`varN` declared with `var`), fields, calls (`super.…`, `super(…)`,
`this(…)`), `new`, arrays, arithmetic, casts (Java's implicit widening left
out), `instanceof`, class literals, string concatenation (`invokedynamic`,
or `StringBuilder` chains), comparisons (NaN kept: `dcmpg; ifge` is
`!(a < b)`), `return`, `throw`; a literal as its target's type writes it
(`true`, `'A'`).

Where blocks join: values two branches of one condition leave are `?:`
(a boolean's `c ? 1 : 0` is `c`); a block that only tests a condition and
shares a target with the test before it joins it as `&&` or `||`; an empty
block that only jumps on is where it jumps. The blocks then nest
(Ramsey's "Beyond Relooper", for graphs whose loops nest): a block inside
the one dominating it; one more than one block reaches, or one reached
leaving a loop, after a labeled block that the jumps to it `break`; a
loop's head a `while (true)` that the jumps back `continue`. Tidying then
drops the jumps falling off a list's end does anyway, retargets a
`break` of a block ending in a loop to the loop, unlabels what the
innermost loop means, moves code reached by one `break` across loops to
that `break`, and forms `while (c)`, `do … while (c)`, `else if` and
`if` without `else` after a branch that never goes on. A variable used in
more than one branch is declared before the first, its stores
assignments. Labels are `label1`, `label2`….

Then javac's shapes are read back as source: `int i = 0; while (i < n) {
…; i++; }` is a `for` (a `continue` going to the update); a `for` over a
hidden array copy and index, or a hidden iterator's `hasNext()`, its
element taken first, is a `for`-each; a `switch` on a hidden copy's
`hashCode()` choosing an index by `equals`, then one on the index, is a
`switch` on strings. Hidden variables are javac's: no name in the table
(`varN`), used nowhere else.

Lambdas: `invokedynamic` through `LambdaMetafactory` to a synthetic
`lambda$…` method of the class is that method decompiled as the lambda's
body (an expression when it is one `return` or call), the values it
captures in place of its first parameters; to any other method, a method
reference (`Type::method`, `receiver::method`, `this::method`,
`super::method`, `Type::new`). javac's boxing (`Integer.valueOf(x)`) and
unboxing (`x.intValue()`) are left out where Java does them by itself — a
return, an assignment, arithmetic, a comparison — and kept in calls'
arguments, where they choose the overload. `new T[] { a, b }` is read from
its element stores (gaps javac leaves the default); `x++` inside an
expression from the load before `iinc`, and a field's `f++`/`++f` from the
value the stack keeps around its store; `x = x + y` is written `x += y`.
A `...` method's last argument, an array made for the call, is written as
the arguments it was — for this class's methods marked varargs and the
JDK's well known ones (`String.format`, `Arrays.asList`, `List.of`,
`printf`…), unless its only element might itself be an array. javac's
`String.valueOf(x)` in a concatenation is just `x`. `if (!X.$assertionsDisabled
&& !c) throw new AssertionError(m)` is `assert c : m;` (the field's setting
in the static initializer left out). A `switch` on an enum: on
`Outer$1.$SwitchMap$…[e.ordinal()]`, its numbers named by that synthetic
class's static initializer; on `e.ordinal()` (an enum of the same file),
by the constants' order when `e`'s type is known. An enum's `values()`,
`valueOf` and the static initializer making its constants are Java's, not
shown.

Layout is the renderer's: four spaces a level, a blank line between
members (but fields in a row), a statement a line; a line past 100
characters breaks after the `(` of the call taking most of it, an argument
a line, eight spaces further in (again for an argument still too long) —
a string or a condition stays whole. No formatter runs.

`try`/`catch`: each range of the exception table is a `try` whose code is
the blocks inside it, entered at its first; its handlers are `catch`es
(one handler's classes `A | B`), the first store of the caught exception
the `catch`'s variable. Like a loop's, code reached leaving it is written
after it. Ranges nest or are apart.

A body is decompiled whole or not at all: anything else — `finally`,
`synchronized` blocks and try-with-resources (handlers catching anything),
two `try`s starting together, values kept on the stack across statements
some other way (`a[i]++` inside an expression), jumps no loop nesting
describes — leaves it compiled code. Nearly all of a Java
library's bodies read (slf4j-api 99.6%, jackson-databind 98%, guava 96%,
commons-lang3 97%). Tests compile
five classes — straight-line code; branches, loops, labeled breaks,
`switch` (fall-through, strings), NaN, `for`-each, array initializers;
lambdas and method references; `try`/`catch` (several, multi-catch, in a
loop, nested, rethrowing); field increments, `assert` (run with `-ea`),
varargs, `switch` on an enum — back from their decompiled text and run a
program against them, which must print the same.

The file starts with a header: where it comes from (the JAR), why (no
sources JAR, or one not declaring it), what is shown (declarations, and
which bodies),
and that the code is its authors', under the library's license. It is
written to `library-sources/<archive>-<hash>/decompiled/<package>/<source
file>` beside extracted sources; its code lens is a banner of the header's
first line (lsp.md). Hover doesn't read it: it says nothing the class files
don't, and has no documentation.

