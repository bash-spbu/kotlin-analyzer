# kotlin-analyzer for Claude Code

Gives Claude Code go-to-definition, references, hover and diagnostics in
Kotlin files (`.kt`, `.kts`) through kotlin-analyzer.

## Install

The plugin runs `kotlin-analyzer` from the `PATH`. Install the server first
(into `~/.local/bin`, which must be on the `PATH`):

```bash
curl -fsSL https://raw.githubusercontent.com/bash-spbu/kotlin-analyzer/main/editors/install-server.sh | sh
```

or build it: `cargo install --git https://github.com/bash-spbu/kotlin-analyzer kotlin-analyzer`.

Then add this repository as a marketplace and install the plugin:

```bash
claude plugin marketplace add bash-spbu/kotlin-analyzer
claude plugin install kotlin-analyzer@kotlin-analyzer
```

Restart Claude Code. Disable other Kotlin language server plugins (such as
`kotlin-lsp`), so that one server answers for `.kt` files.

The server reads the project's model from its Gradle build, the same as in an
editor: see the [main README](../../README.md).
