# LSP

Over stdio, LSP 3.17. Text sync: full documents. Positions use UTF-16
columns. While the workspace is being loaded, `$/progress` reports it under
the token `kotlin-analyzer/loading`, created (`window/workDoneProgress/create`)
right after `initialize` returns. Requests that need the workspace answer
empty until it has loaded.

| request | answers |
|---|---|
| `textDocument/definition`, `declaration` | the declaration(s) the name at the cursor means; in a library, the location inside its source (sources JAR entry or JDK `src.zip` entry, materialised under the cache directory) |
| `textDocument/implementation` | on a class or interface: its subclasses; on a function or property: its overrides |
| `textDocument/hover` | the declaration's signature in a `kotlin` code block, its KDoc/Javadoc, where it is declared (`package.Class`), a deprecation note; on an expression without declaration (`it`, a lambda parameter): its inferred type |
| `textDocument/completion` (+ `completionItem/resolve`) | after `.`: members and applicable extensions of the receiver's type; elsewhere: everything in scope, then importable names with an auto-import edit; inside a call's parentheses: parameter names (`name =`). Ranked by exact prefix, then camel-case humps, then substring. Documentation is filled in on resolve |
| `textDocument/signatureHelp` | the overloads of the call around the cursor, the active one, the active parameter (named arguments respected) |
| `textDocument/references` | every reference to the declaration at the cursor across the workspace (not libraries), optionally with the declaration |
| `textDocument/prepareRename`, `rename` | rename of a local, a parameter, or a workspace declaration with all its references; refused for library declarations and with a message when the target is ambiguous |
| `textDocument/documentHighlight` | references in the current file; the declaration and assignments as *write*, other uses as *read* |
| `textDocument/documentSymbol` | the file's declarations as a tree |
| `workspace/symbol` | workspace declarations (and, for queries of 3+ characters, library classes) matching the query fuzzily |
| `textDocument/foldingRange` | blocks, import list, comment runs |
| `textDocument/semanticTokens/full`, `/range` | classification of identifiers by what they resolve to: class, interface, enum, enumMember, typeParameter, function, method, property, parameter, variable, namespace, annotation; modifiers: declaration, readonly (val), static (companion/object/Java static), deprecated, defaultLibrary (declared in a library) |
| `textDocument/inlayHint` | inferred types of local `val`/`var` (and destructured components) that write no type, except where a constructor call makes it obvious (`val box = Box(…)`); later: lambda parameters and `it`, expression-bodied functions |
| `textDocument/codeAction` | the fixes of the diagnostics in the range: "Import `a.b.C`", "Remove the unused import", "Add `package …`" |
| `textDocument/publishDiagnostics` | see diagnostics.md; published with the document version they were computed for |

`workspace/executeCommand`: `kotlin-analyzer.reloadWorkspace` — reload the
project model and re-read changed files.

Not provided: formatting, type definition, call hierarchy, selection range,
on-type formatting.
