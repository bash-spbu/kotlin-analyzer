//! Library package declarations kept on disk (`docs/spec/libraries.md`):
//! a directory per archive under the store, a file per package holding
//! what it was read from — this version of the server, the archive and its
//! stamp — and the package's item tree. A package is read from here when
//! all of that matches, else from its archive, and written here.
//!
//! Writing is a cache's side effect: the tree is what reading the archive
//! gives, whichever thread or process writes it first.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base_db::{ArchiveStamp, LibrarySource};
use serde::{Deserialize, Serialize};

use crate::item_tree::ItemTree;

/// Where an archive's package trees are, under the store `store`: named by
/// the archive and a hash of what the library is (its archive, kind and
/// sources), so a JDK read for two releases keeps two directories.
pub fn archive_directory(store: &Path, source: &LibrarySource) -> PathBuf {
    let mut hasher = rustc_hash::FxHasher::default();
    source.hash(&mut hasher);
    let name = source
        .archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    store.join(format!("{name}-{:016x}", hasher.finish()))
}

/// The file of `package`'s tree in its archive's directory.
fn package_file(directory: &Path, package: &intern::Path) -> PathBuf {
    let name = if package.segments().is_empty() {
        "_root".to_owned()
    } else {
        package.to_string()
    };
    directory.join(format!("{name}.tree"))
}

/// What a kept tree was read from: equal, the tree is the archive's now.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct MadeFor {
    /// The server's version (an item tree's form changes with it).
    server: String,
    /// The library as `Debug` shows it: archive, kind, sources.
    library: String,
    /// The archive's stamp as `Debug` shows it.
    stamp: String,
}

impl MadeFor {
    fn of(source: &LibrarySource, stamp: ArchiveStamp) -> MadeFor {
        MadeFor {
            server: env!("CARGO_PKG_VERSION").to_owned(),
            library: format!("{source:?}"),
            stamp: format!("{stamp:?}"),
        }
    }
}

/// Where one package's tree is kept, and what it must have been read from.
pub(crate) struct KeptTree {
    file: PathBuf,
    made_for: MadeFor,
}

impl KeptTree {
    pub(crate) fn new(
        store: &Path,
        source: &LibrarySource,
        stamp: ArchiveStamp,
        package: &intern::Path,
    ) -> KeptTree {
        KeptTree {
            file: package_file(&archive_directory(store, source), package),
            made_for: MadeFor::of(source, stamp),
        }
    }

    /// The tree kept, when it was read from the archive as it is now.
    pub(crate) fn read(&self) -> Option<ItemTree> {
        let bytes = std::fs::read(&self.file).ok()?;
        let (made_for, tree): (MadeFor, &[u8]) = postcard::take_from_bytes(&bytes).ok()?;
        if made_for != self.made_for {
            return None;
        }
        ItemTree::from_bytes(tree)
    }

    /// Keeps `tree`: written to a temporary file and renamed, so a reader
    /// finds the whole file or none. A failure leaves nothing to read.
    pub(crate) fn write(&self, tree: &ItemTree) {
        static WRITES: AtomicU64 = AtomicU64::new(0);
        let Ok(mut bytes) = postcard::to_stdvec(&self.made_for) else {
            return;
        };
        let Some(tree_bytes) = tree.to_bytes() else {
            return;
        };
        bytes.extend_from_slice(&tree_bytes);
        let Some(directory) = self.file.parent() else {
            return;
        };
        if std::fs::create_dir_all(directory).is_err() {
            return;
        }
        let write = WRITES.fetch_add(1, Ordering::Relaxed);
        let partial = self
            .file
            .with_extension(format!("partial-{}-{write}", std::process::id()));
        let written = std::fs::write(&partial, bytes).is_ok();
        if !written || std::fs::rename(&partial, &self.file).is_err() {
            let _ = std::fs::remove_file(&partial);
        }
    }
}
