//! Library source entries written where an editor can open them:
//! `$XDG_CACHE_HOME/kotlin-analyzer/library-sources/<archive>/<entry>`.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// The cache directory of the server.
pub fn cache_directory() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
    Some(base.join("kotlin-analyzer"))
}

/// The file holding `entry` of `archive`, written once; `None` when it
/// can't be written.
pub fn materialize(archive: &Path, entry: &str, text: &str) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    archive.hash(&mut hasher);
    let archive_name = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let directory = cache_directory()?
        .join("library-sources")
        .join(format!("{archive_name}-{:016x}", hasher.finish()));
    let path = directory.join(entry);
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref() != Some(text) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, text).ok()?;
    }
    Some(path)
}
