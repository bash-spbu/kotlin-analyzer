# AGENTS.md — adapting kotlin-analyzer to a project

This is for an AI agent asked to make kotlin-analyzer (a Kotlin language
server in Rust) or one of its editor extensions work better for a particular
project or person. Most changes are a few hundred lines in one place: find
the place below, write a failing test, fix, check.

If `AGENTS.local.md` exists next to this file, read it too: it holds the
rules of whoever owns this checkout.

## The loop

1. **Reproduce on the user's project.** Build with `cargo build --release`.
   Then run the binary on the project:
   - `kotlin-analyzer diagnostics <root>` checks every file. It reads
     `.kotlin-analyzer/project.json` when present, else guesses modules
     from the directory layout. `--project-json <file>` gives a model,
     `--only <check,…>` and `--experimental` pick checks, `--limit <n>`
     caps the output.
   - `kotlin-analyzer analysis-stats <root>` reports load time, memory, and
     how many type names resolve.
   - `python3 -I bench/lsp_latency.py <root> --project-json <model>`
     measures editor latencies over stdio.
   - `kotlin-analyzer parse <file>` prints the syntax tree.
   - `kotlin-analyzer definition <root> <project.json> <file> <line>
     <column>` answers go-to-definition.

   The model the editor uses is exported by Gradle into
   `~/.cache/kotlin-analyzer/projects/<root name>-<hash>/project.json`.
   Or export it by hand:
   `./gradlew --no-configuration-cache -Dorg.gradle.isolated-projects=false
   --init-script editors/gradle/kotlin-analyzer.init.gradle
   kotlinAnalyzerModel`.
2. **Write the failing test.** Fixtures are inline multi-file workspaces
   (format in `crates/base-db/src/fixture.rs`):
   - each file starts with `//- /path` and may add `module=:name`,
     `set=jvmMain`, `deps=:other/main` and `classpath=partial`;
   - `$0` marks the cursor;
   - `Fixture::with_mini_stdlib` adds enough of `kotlin.*` for calls and
     generics.

   Helpers: `check_diagnostics`, `check_definition`, `check_completions`,
   `check_signature_help` in `crates/ide/src/tests.rs`; `check_inference`
   and `check_facts` in `crates/hir-ty/src/tests.rs`. Results are
   `expect![[…]]` snapshots; `UPDATE_EXPECT=1 cargo test` fills them in.
   Include the misleading case: a same-named class in another package or
   module, an overload, a generic.
3. **Fix it in the right layer** (map below), then run `cargo xtask check`.
   It checks the crate layering and editor versions, then runs `cargo fmt
   --check`, `cargo clippy -D warnings`, `cargo test` and the Gradle-task
   tests. It must pass.
4. **Check on the project again**, then install: `cargo install --path
   crates/kotlin-analyzer --force`. Every editor extension prefers
   `kotlin-analyzer` on the `PATH` to the release it would download, so the
   editor picks up the new build after a restart of the language server.

## Where things are

| to change… | look in |
|---|---|
| which modules, source sets, dependencies and libraries a build has | `editors/gradle/kotlin-analyzer.init.gradle` (JVM, Kotlin Multiplatform, Android sections); test: `python3 -I editors/gradle/test_init_script.py` with `GRADLE=<gradle>`, fixture in `editors/gradle/fixtures/`. Reading it: `crates/project-model/src/project_json.rs`; the layout guess when there's no model: `convention.rs` |
| a syntax error on valid Kotlin | `crates/syntax/src/lib.rs`: `GrammarGap` (error nodes the grammar leaves on valid code) and `soft_keywords_as_names` (same-length text rewrites before parsing) |
| declarations a file contributes (item trees) | `crates/hir-def/src/item_tree/` (`lower_kotlin.rs`, `lower_java.rs`) |
| what a type or import name means | `crates/hir-def/src/resolver.rs` (imports, default imports, nested and inherited names), `def_map.rs` (visibility, classpath per source set), `expect_actual.rs` (Multiplatform) |
| reading JARs, the JDK's `ct.sym`, Kotlin metadata | `crates/classfile/`, `crates/hir-def/src/library.rs` |
| types, members, overrides, supertypes | `crates/hir-ty/src/members.rs` (overriding: `members/overrides.rs`), `lower.rs`, `signature.rs` |
| what a call or name in a body means | `crates/hir-ty/src/infer/`: `call.rs` (levels in Kotlin's scope order, overloads), `expressions.rs`, `declarations.rs` (locals, receivers, scopes), `narrowing.rs` (smart casts), `flow.rs` (the data flow), `fit.rs`; checks that need only what inference found run after it in `validation.rs`, over `BodyFacts` (syntax + resolved names; `value_path.rs` names values by what they resolve to) |
| a diagnostic | `crates/ide/src/diagnostics/`; `facts.rs` turns inference facts into diagnostics, `imports.rs`, `package.rs`. Spec: `docs/spec/diagnostics.md` |
| an editor feature (hover, completion, rename, …) | `crates/ide/src/<feature>.rs`, exposed by `crates/ide/src/lib.rs`, wired in `crates/kotlin-analyzer/src/server/handlers.rs` / `editing.rs`; capabilities in `server/mod.rs`; settings in `server/settings.rs` |
| loading, reloading, the Gradle export the server runs | `crates/kotlin-analyzer/src/load.rs`, `server/gradle_export.rs` |
| an editor extension | `editors/vscode` (`npm run check`, `npm run test:integration`), `editors/neovim` (`test/run.sh`), `editors/helix`, `editors/zed` (`cargo test`), `editors/claude-code` (`claude plugin validate --strict editors/claude-code`; the marketplace is `.claude-plugin/marketplace.json`). Gradle tasks: `editors/gradle/gradle_task.py`; Zed embeds it: run `python3 -I editors/gradle/zed_tasks.py` after changing it |

`docs/spec/` says what each feature does and when it stays silent. Update it
with the behaviour you change.

## Rules

- **Exact or silent.** When something can't be known for sure, the answer is
  `Unknown`, and no diagnostic is reported. Examples: a library that didn't
  resolve, a lambda whose receiver is unknown, a name a compiler plugin
  generates. A wrong error costs the user more than a missing one. To quiet a
  check on a project, fix the cause, or narrow the check until it's certain.
  Don't loosen what "certain" means.
- **Layers.** Each crate depends only on the layers below it, and `cargo
  xtask` enforces this. Features in `ide` ask `hir`, never `hir-def` or
  `hir-ty` directly. Nothing above `vfs` reads source files from disk.
- **Queries.** Every derived fact is a Salsa query; read paths take
  `&self`. The one exception is syntax trees: `base_db::parse` keeps a
  bounded cache of them, since a tree is a pure function of its text.
- **Every change has a test** that fails without it. Check that: break the
  fix and watch the test fail.
- **Types say what code means.** Reach for an enum or newtype before a
  comment or a `bool`: `enum Classpath { Complete, Partial }`, not
  `complete: bool` plus prose.
- **Full words for names:** `receiver`, `declaration`, `symbol`, never
  `rcv`, `decl`, `s`.
- **Explicit over clever.** Name compound conditions, and prefer a plain
  loop to a long combinator chain. A function named `x_and_y` does two
  things: split it. A section comment inside a body marks a helper waiting
  to be extracted.
- **Comments explain Kotlin,** such as why a rule exists, not what the next
  line does.
- **Write code from the Kotlin specification and `docs/spec/`.** Don't copy
  it from other language servers or IDE plugins; their licences differ.
  Code carried over from earlier work is listed in `PROVENANCE.md`.
