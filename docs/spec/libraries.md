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
which entries hold its declarations. A package's declarations are read when a
query first needs them and kept while recently used. Nothing runs on a JVM.

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
through (`classfile`'s `decompile`, below); every other body is
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

A body is decompiled whole or not at all: anything else — `try`/`catch`,
`synchronized` blocks, lambdas, values kept on the stack across
statements (`i++` inside an expression, array initializers), jumps no loop
nesting describes — leaves it compiled code. Not yet: `for` (a `while`),
`for`-each (its iterator or index), `switch` on strings (the two
switches javac makes). About nine in ten of a Java library's bodies read
(slf4j-api 95%, jackson-databind 92%, guava 90%, commons-lang3 87%).
Kotlin classes keep their bodies out. Tests compile two classes —
straight-line code, and branches, loops, labeled breaks, `switch`
(fall-through, strings), NaN — back from their decompiled text and run a
program against them, which must print the same.

The file starts with a header: where it comes from (the JAR), why (no
sources JAR, or one not declaring it), what is shown (declarations, and
which bodies),
and that the code is its authors', under the library's license. It is
written to `library-sources/<archive>-<hash>/decompiled/<package>/<source
file>` beside extracted sources; its code lens is a banner of the header's
first line (lsp.md). Hover doesn't read it: it says nothing the class files
don't, and has no documentation.

