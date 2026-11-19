//! Kotlin's semantics for coding agents: `kotlin-analyzer mcp` is an MCP
//! server (`mcp`) whose tools ask the host analysing the file's project —
//! the language server an editor runs, found by its socket, or one this
//! process starts when there is none (a git worktree an agent works in,
//! a project no editor has open).

mod debug;
mod host;
pub(crate) mod mcp;
pub(crate) mod protocol;
