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
entries as recorded, types by their simple names with imports, bodies
`{ /* compiled code */ }`, Kotlin parameters with defaults
`/* = … */`. Nothing runs: no JVM, no decompiler of bodies — a reading aid
for a library's API, not its source. A call lands on the constructor
called, a `constructor` of its own (the class files don't say which is
primary).

The file starts with a header: where it comes from (the JAR), why (no
sources JAR, or one not declaring it), that only declarations are shown,
and that the code is its authors', under the library's license. It is
written to `library-sources/<archive>-<hash>/decompiled/<package>/<source
file>` beside extracted sources; its code lens is a banner of the header's
first line (lsp.md). Hover doesn't read it: it says nothing the class files
don't, and has no documentation.

