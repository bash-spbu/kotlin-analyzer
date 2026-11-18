# LSP

Over stdio, LSP 3.17. Text sync: full documents. Positions use UTF-16
columns. While the workspace is being loaded, `$/progress` reports it under
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
| `textDocument/implementation` | on an `expect` declaration (or a use of one): its `actual`s in the module. *(Not yet: on a class or interface, its subclasses; on a function or property, its overrides)* |
| `textDocument/hover` | the declaration's signature in a `kotlin` code block, its KDoc/Javadoc, where it is declared (`package.Class`), a deprecation note; on an expression without declaration (`it`, a lambda parameter): its inferred type |
| `textDocument/completion` | after `.`: the receiver's members — its own class's properties, then its own functions, then inherited properties and functions, `equals`/`hashCode`/`toString` last; a compiled data class's generated `componentN` (for destructuring) isn't offered — Java getters as properties, then the extensions in scope it can use; after `Class.`: nested classes, enum entries, statics, companion members; elsewhere: locals (smart casts applied), the implicit receivers' members, declarations in scope, then — from two typed characters on — declarations not imported yet, with the import as an additional edit; in a type: classes only; in an import: the package's declarations. Matched by prefix, camel-case humps or substring. A name Kotlin reserves is inserted in backticks (`` `object` ``), and so are such segments of an import's path. The scope is the one inference has at the cursor (a placeholder name is parsed there when none is typed yet). *(Later: parameter names, documentation on resolve.)* |
| `textDocument/signatureHelp` | the signatures the callee may have (a class's constructors), the one with room for the argument at the cursor, the active parameter (named arguments and `vararg` respected; arguments counted by the syntax tree, so commas in strings, characters and comparisons don't count) — also while the `)` isn't typed yet |
| `textDocument/references` | every reference to the declaration (or local) at the cursor in the workspace's Kotlin files, optionally with the declaration: files that can see the declaration (its source set visible to theirs, its visibility letting them in) and spell the name are searched, and each occurrence is resolved, so a same-named declaration elsewhere isn't counted. An `expect` declaration and its `actual`s (same module, package, nesting, name and parameter types) are one declaration here. A `val`/`var` primary-constructor parameter and its property are one declaration too: its uses as the parameter (in initializers and defaults), as the property and as a named argument. Names in annotation arguments (`@Import(Config::class)`, constants) are references like any other. *(Not yet: uses from Java files, Java getters used as properties.)* |
| `textDocument/prepareRename`, `rename` | renames a local, a parameter or a Kotlin declaration of the workspace with all its references — a member together with what it overrides and what overrides it in the workspace, an `expect` together with its `actual`s; refused for library and Java declarations (a member overriding one included) and for a name that isn't a plain identifier (keywords included) |
| `textDocument/documentHighlight` | *(not yet)* references in the current file; the declaration and assignments as *write*, other uses as *read* |
| `textDocument/documentSymbol` | the file's declarations as a tree |
| `workspace/symbol` | workspace declarations (and, for queries of 3+ characters, library classes) matching the query fuzzily |
| `textDocument/foldingRange` | multi-line class and function bodies, blocks, lambdas, `when`s, the import list, block comments (a body and its block fold once) |
| `textDocument/semanticTokens/full` | every name classified by what it resolves to: class, interface, enum, enumMember, typeParameter, function, method, property, parameter, variable, namespace, and decorator for an annotation's name where it annotates (`@Name`, `@Name(…)`, `@param:Name`; `Name::class` stays a class or interface); modifiers: declaration, readonly (`val`, non-`var` locals), static, deprecated. *(Later: `/range`, defaultLibrary.)* |
| `textDocument/inlayHint` | inferred types of local `val`/`var` (and destructured components) that write no type, except where a constructor call makes it obvious (`val box = Box(…)`); later: lambda parameters and `it`, expression-bodied functions |
| `textDocument/codeAction` | the fixes of the diagnostics in the range: "Import `a.b.C`", "Remove the unused import", "Add `package …`", "Add the missing branches" (of a `when`, each `case -> TODO()`) |
| `textDocument/publishDiagnostics` | see diagnostics.md; published with the document version they were computed for |

`workspace/executeCommand`: `kotlin-analyzer.reloadWorkspace` — export the
model again if the build files changed, and reload it and every file.

Not provided: formatting, type definition, call hierarchy, selection range,
on-type formatting.
