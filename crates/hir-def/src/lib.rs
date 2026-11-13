//! Definitions and name resolution.
//!
//! - `ItemTree`: one file's declarations with their signatures — no bodies — so
//!   editing inside a function doesn't invalidate anything that only needs
//!   declarations.
//! - `DefMap`: per module, which declarations each package holds and which a
//!   module can see through its dependencies.
//! - Scopes of bodies: which local declaration each name in a body binds to.
