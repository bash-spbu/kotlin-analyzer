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
    let path = directory_of(&cache_directory()?, archive).join(entry);
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref() != Some(text) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, text).ok()?;
    }
    Some(path)
}

/// Where `archive`'s entries are written under the cache directory
/// `cache`: `library-sources/<archive>-<hash>/`.
pub fn directory_of(cache: &Path, archive: &Path) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    archive.hash(&mut hasher);
    let archive_name = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    cache
        .join("library-sources")
        .join(format!("{archive_name}-{:016x}", hasher.finish()))
}

/// Where rendered declarations go: `library-sources/<archive>/decompiled/…`
/// (the analysis names their entries so).
const DECOMPILED_DIRECTORY: &str = "decompiled";

/// For a file of library declarations rendered from class files (one
/// [`materialize`] wrote), its header's first line: where it comes from
/// and why there are no sources. `None` for any other file.
pub fn decompiled_notice(path: &Path) -> Option<String> {
    notice_under(&cache_directory()?.join("library-sources"), path)
}

/// [`decompiled_notice`] of a file under `library_sources`.
fn notice_under(library_sources: &Path, path: &Path) -> Option<String> {
    let inside = path.strip_prefix(library_sources).ok()?;
    let is_decompiled = inside
        .components()
        .nth(1)
        .is_some_and(|component| component.as_os_str() == DECOMPILED_DIRECTORY);
    if !is_decompiled {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?.strip_prefix("// ")?;
    Some(first.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendered_files_notice_is_its_first_line_and_other_files_have_none() {
        let root =
            std::env::temp_dir().join(format!("kotlin-analyzer-notice-{}", std::process::id()));
        let rendered = root.join("okhttp-0123/decompiled/okhttp3/Request.kt");
        let source = root.join("okhttp-sources-0123/okhttp3/Request.kt");
        for (path, text) in [
            (
                &rendered,
                "// Decompiled by kotlin-analyzer from okhttp.jar: no sources.\n// more\n",
            ),
            (&source, "// Decompiled, says a comment\npackage okhttp3\n"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        assert_eq!(
            notice_under(&root, &rendered).as_deref(),
            Some("Decompiled by kotlin-analyzer from okhttp.jar: no sources.")
        );
        assert_eq!(notice_under(&root, &source), None, "sources, not rendered");
        assert_eq!(notice_under(&root.join("elsewhere"), &rendered), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
