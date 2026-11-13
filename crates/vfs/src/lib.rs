//! Virtual file system: every file the server knows gets a `FileId`; text
//! comes from disk or from an open editor buffer (an overlay), and every change
//! is recorded so the database sees one consistent snapshot. Nothing above this
//! crate reads the disk for source text.
//!
//! Also home of `LineIndex`: byte offsets ↔ LSP positions (UTF-16 columns).
