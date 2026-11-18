# kotlin-analyzer for Neovim

Requires Neovim 0.11 or newer. Neovim already ships Kotlin's filetype,
syntax highlighting and indentation; for tree-sitter highlighting add
nvim-treesitter's `kotlin` parser. This plugin adds the language server, for
Kotlin and (read-only) Java, through `vim.lsp.config`. It provides:

- definition into sources and libraries, hover;
- completion with auto-import, signature help;
- references, rename;
- semantic tokens, inlay hints;
- symbols, folding;
- diagnostics with code actions.

## Install

The plugin lives in `editors/neovim` of the repository, so add that
directory to the runtime path.

[lazy.nvim](https://github.com/folke/lazy.nvim):

```lua
{
  "bash-spbu/kotlin-analyzer",
  ft = { "kotlin", "java" },
  config = function(plugin)
    vim.opt.runtimepath:append(plugin.dir .. "/editors/neovim")
    require("kotlin_analyzer").setup()
  end,
}
```

`vim.pack` (Neovim 0.12):

```lua
vim.pack.add({ "https://github.com/bash-spbu/kotlin-analyzer" })
vim.opt.runtimepath:append(vim.fn.stdpath("data") .. "/site/pack/core/opt/kotlin-analyzer/editors/neovim")
require("kotlin_analyzer").setup()
```

`setup()` uses the first server binary it finds:

1. `cmd`, if given;
2. `kotlin-analyzer` on the `PATH`;
3. the release matching the plugin's version, already downloaded under
   `stdpath("data")/kotlin-analyzer/<version>/`.

If none of these exists, it downloads that release from GitHub, checks its
SHA-256, and starts the server once the download is done. Downloading needs
`curl`, `gzip` and `shasum`/`sha256sum`. Releases exist for macOS and Linux
on arm64 and x86_64. Elsewhere, build the server with
`cargo install --path crates/kotlin-analyzer`.

The server already on the `PATH` is enough without `setup()`: the plugin's
`lsp/kotlin_analyzer.lua` holds the defaults, so this works on its own:

```lua
vim.lsp.enable("kotlin_analyzer")
```

The server opens the Gradle build's root: the nearest directory with a
`settings.gradle(.kts)`, else the git root. It exports the build's model in
the background.

## Settings

```lua
require("kotlin_analyzer").setup({
  cmd = nil,            -- e.g. { "/path/to/kotlin-analyzer" }
  download = true,      -- download the release when there's no binary
  java = true,          -- also serve Java buffers (false next to jdtls)
  init_options = {
    diagnostics = { disabled = { "unused-import" }, experimental = false },
    gradleExport = true,
    projectJson = nil,  -- a model exported beforehand
  },
  lsp = {},             -- anything else for vim.lsp.config: on_attach, capabilities, …
})
```

Diagnostics checks: `syntax`, `unresolved-reference`, `missing-import`,
`argument-mismatch`, `unsafe-call`, `non-exhaustive-when`, `unused-import`,
`missing-package`, `unnecessary-not-null-assertion`, `unnecessary-safe-call`,
`useless-elvis`, `senseless-comparison`, `unused-variable`, `can-be-val`,
`deprecation`, `deprecation-error`, `opt-in-usage`, `opt-in-usage-error`.

Inlay hints are off by default in Neovim; turn them on with
`vim.lsp.inlay_hint.enable()`.

## Commands

- `:KotlinGradle build` runs `gradle build -x test`, `:KotlinGradle clean`
  runs `gradle clean`.
- `:KotlinGradle test [project]` runs `gradle test --rerun-tasks` in the
  current file's project and its subprojects. The optional `[project]` is the
  file's project or a parent, by Gradle path; `<Tab>` completes them
  (`:services:payments`, `:services`, `:`). This is how to test a group of
  projects.

  Gradle runs in a terminal split, in the build's root. The project holding the file is its nearest ancestor directory with a
  `build.gradle(.kts)`, and the build's root is the nearest one with a
  `settings.gradle(.kts)`. Gradle is the root's `gradlew`, else `gradle` on
  the `PATH`. `python3` must be on the `PATH`; the tasks are run by
  `editors/gradle/gradle_task.py`.

- `:KotlinAnalyzerReload` exports the model again if the build changed, then
  reloads every file.
- `:KotlinAnalyzerInstall` downloads the plugin's release again.
- `:lsp restart` (Neovim 0.12) restarts the server.

## Tests

`test/run.sh` runs the plugin in a clean headless Neovim, with no server on
the `PATH`. It serves `target/release/kotlin-analyzer` as the release, so build
that first. The run checks:

- the download, and the refusal of a tampered release;
- that the server opens the build's root;
- diagnostics, hover, definition and reload.
