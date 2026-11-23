//! Library indexes kept on disk (`docs/spec/libraries.md`): one file per
//! archive under `library-index/`, holding what it was made from — the
//! archive, its kind and sources, their stamp, the server's version — and
//! the index. A load takes an index from here when all of that matches,
//! else indexes the archive and writes the file.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Where the index files are, under the cache directory.
pub fn directory(cache: &Path) -> PathBuf {
    cache.join("library-index")
}

/// The index of `source` kept in `directory`, when it was made from the
/// archive as it is now (`stamp`) by this version of the server.
pub fn read(
    directory: &Path,
    source: &hir::LibrarySource,
    stamp: hir::ArchiveStamp,
) -> Option<hir::IndexedLibrary> {
    let bytes = std::fs::read(file_of(directory, source)).ok()?;
    let cached: CachedIndex = postcard::from_bytes(&bytes).ok()?;
    let made_for = MadeFor::of(source, stamp);
    (cached.made_for == made_for).then(|| cached.index.into_indexed(stamp))
}

/// Keeps `indexed`, the index of `source`, in `directory`: written to a
/// temporary file and renamed, so a reader finds the whole file or none.
/// A failure leaves nothing to read (the next load indexes again).
pub fn write(directory: &Path, source: &hir::LibrarySource, indexed: &hir::IndexedLibrary) {
    let cached = CachedIndex {
        made_for: MadeFor::of(source, indexed.stamp),
        index: CachedPackages::of(indexed),
    };
    let Ok(bytes) = postcard::to_stdvec(&cached) else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let file = file_of(directory, source);
    let partial = file.with_extension(format!("partial-{}", std::process::id()));
    let written = std::fs::write(&partial, bytes).is_ok();
    if !written || std::fs::rename(&partial, &file).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// The file of `source`'s index: named by a hash of where its archive is.
pub fn file_of(directory: &Path, source: &hir::LibrarySource) -> PathBuf {
    let mut hasher = rustc_hash::FxHasher::default();
    source.archive.hash(&mut hasher);
    let name = source
        .archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    directory.join(format!("{name}-{:016x}.index", hasher.finish()))
}

#[derive(Serialize, Deserialize)]
struct CachedIndex {
    made_for: MadeFor,
    index: CachedPackages,
}

/// What an index was made from: equal, the index is the archive's now.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct MadeFor {
    /// The server's version: another may index differently.
    server: String,
    archive: String,
    /// The archive's kind as `Debug` shows it (`Jar`, `JdkSymbols { … }`).
    kind: String,
    sources: Option<String>,
    /// Modification time since the epoch (seconds, nanoseconds), when known.
    modified: Option<(u64, u32)>,
    length: u64,
}

impl MadeFor {
    fn of(source: &hir::LibrarySource, stamp: hir::ArchiveStamp) -> MadeFor {
        let modified = stamp
            .modified
            .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|since| (since.as_secs(), since.subsec_nanos()));
        MadeFor {
            server: env!("CARGO_PKG_VERSION").to_owned(),
            archive: source.archive.to_string_lossy().into_owned(),
            kind: format!("{:?}", source.kind),
            sources: source
                .sources
                .as_ref()
                .map(|sources| sources.to_string_lossy().into_owned()),
            modified,
            length: stamp.length,
        }
    }
}

/// An index's packages, names as text (interned names are this process's).
#[derive(Serialize, Deserialize)]
struct CachedPackages {
    packages: Vec<CachedPackage>,
}

#[derive(Serialize, Deserialize)]
struct CachedPackage {
    package: Vec<String>,
    names: Vec<String>,
    entries: Vec<u32>,
    module: Option<String>,
}

impl CachedPackages {
    fn of(indexed: &hir::IndexedLibrary) -> CachedPackages {
        let packages = indexed
            .packages
            .iter()
            .map(|package| CachedPackage {
                package: package
                    .package
                    .segments()
                    .iter()
                    .map(|segment| segment.as_str().to_owned())
                    .collect(),
                names: package
                    .names
                    .iter()
                    .map(|name| name.as_str().to_owned())
                    .collect(),
                entries: package.entries.clone(),
                module: package.module.map(|module| module.as_str().to_owned()),
            })
            .collect();
        CachedPackages { packages }
    }

    fn into_indexed(self, stamp: hir::ArchiveStamp) -> hir::IndexedLibrary {
        let packages = self
            .packages
            .into_iter()
            .map(|package| hir::IndexedPackage {
                package: hir::Path::new(
                    package
                        .package
                        .iter()
                        .map(|segment| hir::Name::new(segment)),
                ),
                names: package
                    .names
                    .iter()
                    .map(|name| hir::Name::new(name))
                    .collect(),
                entries: package.entries,
                module: package.module.map(|module| hir::Name::new(&module)),
            })
            .collect();
        hir::IndexedLibrary { packages, stamp }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "kotlin-analyzer-index-cache-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    fn indexed(length: u64) -> hir::IndexedLibrary {
        hir::IndexedLibrary {
            packages: vec![hir::IndexedPackage {
                package: hir::Path::parse("com.example"),
                names: vec![hir::Name::new("Client"), hir::Name::new("connect")],
                entries: vec![3, 7],
                module: Some(hir::Name::new("java.base")),
            }],
            stamp: hir::ArchiveStamp {
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                length,
            },
        }
    }

    #[test]
    fn an_index_comes_back_for_the_same_archive_as_it_was_and_nothing_else() {
        let directory = scratch("round-trip");
        let source = hir::LibrarySource {
            archive: PathBuf::from("/m2/client-1.0.jar"),
            kind: hir::ArchiveKind::Jar,
            sources: Some(PathBuf::from("/m2/client-1.0-sources.jar")),
        };
        let kept = indexed(42);
        write(&directory, &source, &kept);
        assert_eq!(read(&directory, &source, kept.stamp), Some(kept.clone()));
        // Rebuilt in place: another length.
        let rebuilt = hir::ArchiveStamp {
            length: 43,
            ..kept.stamp
        };
        assert_eq!(read(&directory, &source, rebuilt), None);
        // Another sources JAR, or another kind: not this index.
        let other_sources = hir::LibrarySource {
            sources: None,
            ..source.clone()
        };
        assert_eq!(read(&directory, &other_sources, kept.stamp), None);
        let as_jdk = hir::LibrarySource {
            kind: hir::ArchiveKind::JdkSymbols {
                release: 'P',
                known: hir::JdkRelease::Declared,
            },
            ..source.clone()
        };
        assert_eq!(read(&directory, &as_jdk, kept.stamp), None);
        // A file that isn't an index: ignored.
        std::fs::write(file_of(&directory, &source), b"garbage").unwrap();
        assert_eq!(read(&directory, &source, kept.stamp), None);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
