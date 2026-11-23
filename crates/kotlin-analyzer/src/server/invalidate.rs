//! *Invalidate caches and restart* (`docs/spec/lsp.md`): which of the cache
//! directory's files and directories are the project's, and deleting them.
//! What the server keeps in memory is dropped when the fresh load that
//! follows is applied (`GlobalState::on_loaded`).

use std::path::{Path, PathBuf};

/// The project's caches under the cache directory `cache`: each exported
/// model's directory (`exported_models` are the model files), and, of each
/// library, its index file, its package declarations' directory and the
/// directories its archive's and its sources' entries are written to.
pub(super) fn project_caches(
    cache: &Path,
    exported_models: &[PathBuf],
    libraries: &[hir::LibrarySource],
) -> Vec<PathBuf> {
    let index_directory = crate::library_index_cache::directory(cache);
    let tree_store = crate::load::library_tree_store(cache);
    let mut caches: Vec<PathBuf> = exported_models
        .iter()
        .filter_map(|model| model.parent().map(Path::to_path_buf))
        .collect();
    for library in libraries {
        caches.push(crate::library_index_cache::file_of(
            &index_directory,
            library,
        ));
        caches.push(hir::library_tree_directory(&tree_store, library));
        let archives = std::iter::once(&library.archive).chain(library.sources.as_ref());
        caches.extend(archives.map(|archive| super::library_sources::directory_of(cache, archive)));
    }
    caches.sort();
    caches.dedup();
    caches
}

/// Deletes `caches` (files or directories); how many were there.
pub(super) fn delete(caches: &[PathBuf]) -> usize {
    caches
        .iter()
        .filter(|cache| {
            let removed = if cache.is_dir() {
                std::fs::remove_dir_all(cache)
            } else {
                std::fs::remove_file(cache)
            };
            removed.is_ok()
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_projects_caches_are_its_models_and_its_libraries_files_only() {
        let cache = PathBuf::from("/home/me/.cache/kotlin-analyzer");
        let model = cache.join("projects/shop-0123456789abcdef/project.json");
        let jar = hir::LibrarySource {
            archive: PathBuf::from("/m2/client-1.0.jar"),
            kind: hir::ArchiveKind::Jar,
            sources: Some(PathBuf::from("/m2/client-1.0-sources.jar")),
        };
        let caches = project_caches(&cache, &[model], &[jar]);
        let shown: Vec<String> = caches
            .iter()
            .map(|path| path.strip_prefix(&cache).unwrap().display().to_string())
            .map(|path| {
                // Hashes vary by platform: the name before its hash.
                path.rsplit_once('-')
                    .map_or(path.clone(), |(start, _)| start.to_owned())
            })
            .collect();
        assert_eq!(
            shown,
            [
                "library-index/client-1.0.jar",
                "library-sources/client-1.0",
                "library-sources/client-1.0-sources",
                "library-trees/client-1.0.jar",
                "projects/shop",
            ]
        );
        assert!(
            caches.iter().all(|path| path.starts_with(&cache)),
            "nothing outside the cache"
        );
    }

    #[test]
    fn deleting_removes_files_and_directories_and_counts_what_was_there() {
        let root =
            std::env::temp_dir().join(format!("kotlin-analyzer-invalidate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("projects/shop/nested")).unwrap();
        std::fs::write(root.join("projects/shop/nested/project.json"), "{}").unwrap();
        std::fs::write(root.join("client.index"), "index").unwrap();
        let removed = delete(&[
            root.join("projects/shop"),
            root.join("client.index"),
            root.join("never-there"),
        ]);
        assert_eq!(removed, 2);
        assert!(!root.join("projects/shop").exists());
        assert!(!root.join("client.index").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
