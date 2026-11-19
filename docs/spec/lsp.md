# LSP

Over stdio, LSP 3.17. Text sync: incremental (each change replaces a
range, as the changes before it left the text; one without a range replaces
it all). Positions use UTF-8 columns (bytes) when the client offers them
(`general.positionEncodings`), else UTF-16. The workspace is every
workspace folder (`workspaceFolders`; else the root URI) read as one — each
folder's model (its exported Gradle model, its own `project.json`, or its
directories), the first folder the primary one (the given `projectJson`'s,
the log's); `workspace/didChangeWorkspaceFolders` reads it again from the
folders there are then. `$/cancelRequest` answers a request
not answered yet with `RequestCancelled` at once: its work is skipped when
it hasn't started, and its result dropped when it comes (a computation
already running goes on until done or an edit cancels it). While the workspace is being loaded, `$/progress` reports it under
the token `kotlin-analyzer/loading`, created (`window/workDoneProgress/create`)
right after `initialize` returns. Requests that need the workspace answer
empty until it has loaded; once it has, the client is asked to refresh
semantic tokens and inlay hints (when it supports `refresh`).

The server asks the client to watch `**/*.kt`, `**/*.java`, the Gradle
build files and the model file (`workspace/didChangeWatchedFiles`, when the
client supports registering it): sources changed on disk and not open in
the editor are read again, new ones added and deleted ones dropped; a
changed build file exports the model again if it's stale, a changed model
reloads the workspace. An edit re-checks every open file (the edited one
first): what one file declares changes what another's code means.

`exit` without a `shutdown` before it ends the process with exit code 1.

| request | answers |
|---|---|
| `textDocument/definition` | the declaration(s) the name at the cursor means; in a library, the location inside its source (sources JAR entry or JDK `src.zip` entry, materialised under the cache directory) |
| `textDocument/declaration` | as the definition, but an `actual` declaration's (or a use of one) is its `expect` |
| `textDocument/prepareTypeHierarchy`, `typeHierarchy/supertypes`, `typeHierarchy/subtypes` | the class at the cursor; its direct supertypes (a library's too, and the implicit `Any`); the workspace classes that name it among their supertypes. An item's `data` holds its package and names, resolved again from the file the hierarchy started in |
| `textDocument/prepareCallHierarchy`, `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls` | the function or constructor at the cursor; the calls of it, grouped by the function, constructor or property each is in; the functions and constructors it calls, grouped by callee, with where. A workspace item's `data` holds where its name is; a library's isn't expanded |
| `textDocument/diagnostic`, `workspace/diagnostic` | pull diagnostics (see diagnostics.md): a file's, or every workspace file's (checked in parallel); the report's id is the workspace's revision, so a report asked again before anything changed is "unchanged". A client that declares pull support isn't sent diagnostics (`publishDiagnostics`); it's asked to pull again (`workspace/diagnostic/refresh`, when it supports it) after an edit, a load or a settings change |
| `textDocument/typeDefinition` | the classes of the type of the name at the cursor — a local, a parameter, a property or an expression's name: its class, then its type arguments' (`List<Order>`: `List`, `Order`) |
| `textDocument/implementation` | on a class or interface (or a use of one): its subclasses in the workspace, at any depth, Java ones included; on a function or property: its overrides in those (by name, kind and JVM signature — an overload isn't one); on an `expect` declaration: its `actual`s in the module |
| `textDocument/hover` | the declaration's signature in a `kotlin` code block (over 60 characters with several parameters, a parameter per line), its KDoc/Javadoc, where it is declared (`package.Class`), a deprecation note; on an expression without declaration (`it`, a lambda parameter): its inferred type |
| `textDocument/completion` | after `.`: the receiver's members — its own class's properties, then its own functions, then inherited properties and functions, `equals`/`hashCode`/`toString` last; a compiled data class's generated `componentN` (for destructuring) isn't offered — Java getters as properties, then the extensions in scope it can use; after `Class.`: nested classes, enum entries, statics, companion members; elsewhere: locals (smart casts applied), the implicit receivers' members, declarations in scope, then — from two typed characters on — declarations not imported yet, with the import as an additional edit; in a type: classes only; in an import: the package's declarations. Matched by prefix, camel-case humps or substring. A name Kotlin reserves is inserted in backticks (`` `object` ``), and so are such segments of an import's path. The scope is the one inference has at the cursor (a placeholder name is parsed there when none is typed yet). Inside a call's parentheses, where an argument starts: `name = ` first for each parameter of the callee's signatures not passed by name yet. `completionItem/resolve` gives a declaration's item, as documentation, its whole signature (a `kotlin` code block; over 60 characters with several parameters, a parameter per line, which a menu too narrow for the label can't show), its KDoc/Javadoc and where it's declared. An item's `labelDetails` show the rest of its signature after its name (`(amount: Long): Unit`, `: String`) and, apart, where it's declared (its class, its package) or the import accepting it adds. |
| `textDocument/signatureHelp` | the signatures the callee may have (a class's constructors), the one with room for the argument at the cursor, the active parameter (named arguments and `vararg` respected; arguments counted by the syntax tree, so commas in strings, characters and comparisons don't count) — also while the `)` isn't typed yet |
| `textDocument/references` | every reference to the declaration (or local) at the cursor in the workspace's Kotlin files, optionally with the declaration: files that can see the declaration (its source set visible to theirs, its visibility letting them in) and spell the name are searched, and each occurrence is resolved, so a same-named declaration elsewhere isn't counted. An `expect` declaration and its `actual`s (same module, package, nesting, name and parameter types) are one declaration here. A `val`/`var` primary-constructor parameter and its property are one declaration too: its uses as the parameter (in initializers and defaults), as the property and as a named argument. Names in annotation arguments (`@Import(Config::class)`, constants) are references like any other. A Java getter is found by both its names: `getTitle()` and the property `title` Kotlin reads it as. *(Not yet: uses from Java files.)* |
| `textDocument/prepareRename`, `rename` | renames a local, a parameter or a Kotlin declaration of the workspace with all its references — a member together with what it overrides and what overrides it in the workspace, an `expect` together with its `actual`s; refused for library and Java declarations (a member overriding one included) and for a name that isn't a plain identifier (keywords included) |
| `textDocument/documentHighlight` | the occurrences in the current file of what the name at the cursor means, found as references are; the declaration, an assignment's target (`x = …`, `x += …`, `this.x = …`) and `x++`/`--x` as *write*, the rest — `x` of `x.y = …` included — as *read* |
| `textDocument/documentSymbol` | the file's declarations as a tree |
| `workspace/symbol` | workspace declarations (and, for queries of 3+ characters, library classes) matching the query fuzzily |
| `textDocument/selectionRange` | for each position, the ranges "expand selection" grows through, innermost first: the word at it (inside a string or a comment too; at a word's end before punctuation, that word), then each syntax node around it, with what lies between a node's brackets or quotes (`(…)`, `{…}`, `[…]`, `<…>`, `"…"`, whitespace by them left out) before the node itself |
| `textDocument/codeLens`, `codeLens/resolve` | in a Kotlin file, once the workspace has loaded: over each class, function, property and type alias, its reference count — counted on `codeLens/resolve` (`data` holds where its name is), as `textDocument/references` would find them, with the command `kotlin-analyzer.showReferences` (arguments: the file's URI, the name's position, the references' locations); over each function annotated as a test (JUnit 4 and 5's `@Test`, `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`, `@TestTemplate`, kotlin.test's and TestNG's `@Test`: by what the annotation resolves to, or without the library on the classpath by its import or qualified spelling) "▶ Run test", and over each class holding one (itself or in a nested class) "▶ Run tests", with the command `kotlin-analyzer.runTests` and `{ "uri", "filter" }` — Gradle's `--tests` filter (`app.OrderTest.creates an order`, `app.Outer$Nested`). The commands are the client's to run (the editor extensions do). `codeLens.references` and `codeLens.tests` (both on by default) turn either kind off; the client is asked to refresh its lenses (`workspace/codeLens/refresh`, when it supports it) after a load and a change of these |
| `textDocument/foldingRange` | multi-line class and function bodies, blocks, lambdas, `when`s, the import list, block comments (a body and its block fold once) |
| `textDocument/semanticTokens/full` | every name classified by what it resolves to: class, interface, enum, enumMember, typeParameter, function, method, property, parameter, variable, namespace, and decorator for an annotation's name where it annotates (`@Name`, `@Name(…)`, `@param:Name`; `Name::class` stays a class or interface); modifiers: declaration, readonly (`val`, non-`var` locals), static, deprecated, mutable (`var`), defaultLibrary (Kotlin's standard library, the JDK: `kotlin.*`, `java.*`, `javax.*`). |
| `textDocument/semanticTokens/full/delta` | what changed since the tokens sent with `previousResultId` (the last sent for the file): one edit between their common start and end; all the tokens when that id isn't the last |
| `textDocument/semanticTokens/range` | the tokens overlapping the range |
| `workspace/willRenameFiles` | for Kotlin files and folders (a folder: every Kotlin file under it), the edits that keep the code right, made to the files before they move: a file renamed in its directory renames the one top-level class named as it was (`Order.kt` → `Invoice.kt` renames `class Order` and its uses); a file moved where its new directory implies another package — its package having been its old directory's — gets that `package` line, other files' imports and fully qualified names of its non-private top-level declarations follow, a file that used them without an import (same package, a star import) gets one, and the moved file gets imports of what it used from its old package; files moved together need no imports of each other |
| `textDocument/inlayHint` | inferred types of what writes none: local `val`/`var` (and destructured components), except where a constructor call makes it obvious (`val box = Box(…)`); a lambda's untyped parameters; `it: T` after the `{` of a lambda with an implicit `it`; an expression-bodied function's result after its parameters (`fun total() = …`). Each class name in a hint is a label part linked to its declaration — a workspace class's at once, a library's on `inlayHint/resolve` (read from its sources archive when it has one) |
| `textDocument/codeAction` | the fixes of the diagnostics in the range: "Import `a.b.C`", "Remove the unused import", "Add `package …`", "Add the missing branches" (of a `when`, each `case -> TODO()`) |
| `textDocument/formatting` | a Kotlin file (`.kt`, `.kts`) formatted by an external formatter — ktrs (the default; ktfmt's output from a native binary) or ktfmt — in the style set (`kotlinlang` by default, `meta`, `google`): the document's text goes to its stdin (`ktrs fmt --style <style> -`, `ktfmt --<style>-style -`, or `java -jar <ktfmt.jar> --<style>-style -` for a jar), its stdout is the formatted text, and the answer is one edit between their common start and end (none when nothing changed). The formatter is stopped after 20 s. A formatter that isn't found, fails or refuses the file (a syntax error) answers `RequestFailed` with why — the formatter's own message, which may quote the code, goes only to the user, never to the log. Java files, and every file with `formatting.formatter: "off"`, get no edits. The editor extensions download the formatter (pinned versions, SHA-256 checked) and pass its path |
| `textDocument/publishDiagnostics` | see diagnostics.md; published with the document version they were computed for |

`workspace/didRenameFiles` (Kotlin files and folders): the old paths' files
are dropped and the new ones read (a file open in the editor is the
editor's to report).

`workspace/executeCommand`: `kotlin-analyzer.reloadWorkspace` — export the
model again if the build files changed, and reload it and every file.

Settings in `initializationOptions`: `diagnostics.disabled`,
`diagnostics.experimental`, `formatting.formatter` (`"ktrs"`, `"ktfmt"`,
`"off"`), `formatting.style` (`"kotlinlang"`, `"meta"`, `"google"`),
`formatting.path` (the formatter's executable or ktfmt's `.jar`, run with
`$JAVA_HOME/bin/java` or `java`; else the formatter's name on the `PATH`),
`codeLens.references`, `codeLens.tests` (booleans, `true` by default),
`gradleExport`, `projectJson`, and `log` (`"info"`, `"debug"`; see
logging.md). A value not understood is the default. `diagnostics`,
`formatting`, `codeLens` and `log` may also change in `workspace/didChangeConfiguration`
(under `kotlin-analyzer`, or at the top); a `formatting` there replaces the
one before as a whole.

Not provided: range formatting, on-type formatting.
