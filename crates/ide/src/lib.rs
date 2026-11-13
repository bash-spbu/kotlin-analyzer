//! Editor features as pure functions over a database snapshot: navigation,
//! hover, completion, references, rename, highlighting, symbols, code actions
//! and diagnostics. Each feature asks `hir`; none digs into lower layers.
