# LSP

Over stdio, LSP 3.17. Text sync: incremental (each change replaces a
range, as the changes before it left the text; one without a range replaces
it all). Positions use UTF-16 columns. `$/cancelRequest` answers a request
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
| `textDocument/definition`, `declaration` | the declaration(s) the name at the cursor means; in a library, the location inside its source (sources JAR entry or JDK `src.zip` entry, materialised under the cache directory) |
| `textDocument/typeDefinition` | the classes of the type of the name at the cursor — a local, a parameter, a property or an expression's name: its class, then its type arguments' (`List<Order>`: `List`, `Order`) |
| `textDocument/implementation` | on a class or interface (or a use of one): its subclasses in the workspace, at any depth, Java ones included; on a function or property: its overrides in those (by name, kind and JVM signature — an overload isn't one); on an `expect` declaration: its `actual`s in the module |
| `textDocument/hover` | the declaration's signature in a `kotlin` code block, its KDoc/Javadoc, where it is declared (`package.Class`), a deprecation note; on an expression without declaration (`it`, a lambda parameter): its inferred type |
| `textDocument/completion` | after `.`: the receiver's members — its own class's properties, then its own functions, then inherited properties and functions, `equals`/`hashCode`/`toString` last; a compiled data class's generated `componentN` (for destructuring) isn't offered — Java getters as properties, then the extensions in scope it can use; after `Class.`: nested classes, enum entries, statics, companion members; elsewhere: locals (smart casts applied), the implicit receivers' members, declarations in scope, then — from two typed characters on — declarations not imported yet, with the import as an additional edit; in a type: classes only; in an import: the package's declarations. Matched by prefix, camel-case humps or substring. A name Kotlin reserves is inserted in backticks (`` `object` ``), and so are such segments of an import's path. The scope is the one inference has at the cursor (a placeholder name is parsed there when none is typed yet). Inside a call's parentheses, where an argument starts: `name = ` first for each parameter of the callee's signatures not passed by name yet. `completionItem/resolve` gives a declaration's item its KDoc/Javadoc. |
| `textDocument/signatureHelp` | the signatures the callee may have (a class's constructors), the one with room for the argument at the cursor, the active parameter (named arguments and `vararg` respected; arguments counted by the syntax tree, so commas in strings, characters and comparisons don't count) — also while the `)` isn't typed yet |
| `textDocument/references` | every reference to the declaration (or local) at the cursor in the workspace's Kotlin files, optionally with the declaration: files that can see the declaration (its source set visible to theirs, its visibility letting them in) and spell the name are searched, and each occurrence is resolved, so a same-named declaration elsewhere isn't counted. An `expect` declaration and its `actual`s (same module, package, nesting, name and parameter types) are one declaration here. A `val`/`var` primary-constructor parameter and its property are one declaration too: its uses as the parameter (in initializers and defaults), as the property and as a named argument. Names in annotation arguments (`@Import(Config::class)`, constants) are references like any other. A Java getter is found by both its names: `getTitle()` and the property `title` Kotlin reads it as. *(Not yet: uses from Java files.)* |
| `textDocument/prepareRename`, `rename` | renames a local, a parameter or a Kotlin declaration of the workspace with all its references — a member together with what it overrides and what overrides it in the workspace, an `expect` together with its `actual`s; refused for library and Java declarations (a member overriding one included) and for a name that isn't a plain identifier (keywords included) |
| `textDocument/documentHighlight` | the occurrences in the current file of what the name at the cursor means, found as references are; the declaration, an assignment's target (`x = …`, `x += …`, `this.x = …`) and `x++`/`--x` as *write*, the rest — `x` of `x.y = …` included — as *read* |
| `textDocument/documentSymbol` | the file's declarations as a tree |
| `workspace/symbol` | workspace declarations (and, for queries of 3+ characters, library classes) matching the query fuzzily |
| `textDocument/foldingRange` | multi-line class and function bodies, blocks, lambdas, `when`s, the import list, block comments (a body and its block fold once) |
| `textDocument/semanticTokens/full` | every name classified by what it resolves to: class, interface, enum, enumMember, typeParameter, function, method, property, parameter, variable, namespace, and decorator for an annotation's name where it annotates (`@Name`, `@Name(…)`, `@param:Name`; `Name::class` stays a class or interface); modifiers: declaration, readonly (`val`, non-`var` locals), static, deprecated. *(Later: defaultLibrary.)* |
| `textDocument/semanticTokens/full/delta` | what changed since the tokens sent with `previousResultId` (the last sent for the file): one edit between their common start and end; all the tokens when that id isn't the last |
| `textDocument/semanticTokens/range` | the tokens overlapping the range |
| `textDocument/inlayHint` | inferred types of what writes none: local `val`/`var` (and destructured components), except where a constructor call makes it obvious (`val box = Box(…)`); a lambda's untyped parameters; `it: T` after the `{` of a lambda with an implicit `it`; an expression-bodied function's result after its parameters (`fun total() = …`). Each class name in a hint is a label part linked to its declaration — a workspace class's at once, a library's on `inlayHint/resolve` (read from its sources archive when it has one) |
| `textDocument/codeAction` | the fixes of the diagnostics in the range: "Import `a.b.C`", "Remove the unused import", "Add `package …`", "Add the missing branches" (of a `when`, each `case -> TODO()`) |
| `textDocument/publishDiagnostics` | see diagnostics.md; published with the document version they were computed for |

`workspace/executeCommand`: `kotlin-analyzer.reloadWorkspace` — export the
model again if the build files changed, and reload it and every file.

Settings in `initializationOptions`: `diagnostics.disabled`,
`diagnostics.experimental`, `gradleExport`, `projectJson`, and `log`
(`"info"`, `"debug"`; see logging.md). `diagnostics` and `log` may also
change in `workspace/didChangeConfiguration` (under `kotlin-analyzer`).

Not provided: formatting, call hierarchy, selection range,
on-type formatting.
