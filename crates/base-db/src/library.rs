//! Library inputs: each compiled archive, the packages it holds, and which
//! source set sees which library.
//!
//! An archive is indexed once (top-level names per package, and where each
//! package's classes are in it); a package's declarations are read only when
//! a query needs them.

use std::path::PathBuf;
use std::sync::Arc;

use intern::{Name, Path};
use rustc_hash::FxHashMap;

/// What an archive is.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ArchiveKind {
    /// A JAR of `.class` files (and Kotlin `.kotlin_builtins`).
    Jar,
    /// The JDK's `lib/ct.sym`: `<releases>/<module>/<package>/<Class>.sig`,
    /// read for the release whose letter is given (`P` for 25).
    JdkSymbols { release: char },
}

/// Where a library's declarations and sources come from.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LibrarySource {
    pub archive: PathBuf,
    pub kind: ArchiveKind,
    /// The sources JAR, or the JDK's `src.zip`.
    pub sources: Option<PathBuf>,
}

/// A library as the indexer leaves it: per package (sorted), its top-level
/// names and the archive entries that hold its declarations; and the
/// archive's state when it was read.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct IndexedLibrary {
    pub packages: Vec<IndexedPackage>,
    pub stamp: ArchiveStamp,
}

/// An archive file's modification time and length: a rebuilt archive at
/// the same path has another.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ArchiveStamp {
    pub modified: Option<std::time::SystemTime>,
    pub length: u64,
}

impl ArchiveStamp {
    pub fn of(path: &std::path::Path) -> ArchiveStamp {
        match std::fs::metadata(path) {
            Ok(metadata) => ArchiveStamp {
                modified: metadata.modified().ok(),
                length: metadata.len(),
            },
            Err(_) => ArchiveStamp::default(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct IndexedPackage {
    pub package: Path,
    /// Top-level classes, functions, properties and type aliases, sorted.
    pub names: Vec<Name>,
    /// Indices of the archive entries of this package.
    pub entries: Vec<u32>,
    /// For the JDK: the module the package belongs to (`java.base`), which
    /// names its directory in `src.zip`.
    pub module: Option<Name>,
}

/// One library archive.
#[salsa::input(debug)]
pub struct Library {
    #[returns(ref)]
    pub source: Arc<LibrarySource>,
    #[returns(ref)]
    pub packages: Arc<FxHashMap<Path, LibraryPackage>>,
    /// The archive as indexed. Reading a package's declarations depends on
    /// it, so an archive rewritten in place is read again after a reload.
    #[returns(copy)]
    pub stamp: ArchiveStamp,
}

/// One package of one library — the key of the query that reads its
/// declarations.
#[salsa::input(debug)]
pub struct LibraryPackage {
    #[returns(copy)]
    pub library: Library,
    #[returns(ref)]
    pub package: Path,
    /// Sorted, for binary search.
    #[returns(ref)]
    pub names: Arc<[Name]>,
    #[returns(ref)]
    pub entries: Arc<[u32]>,
    #[returns(copy)]
    pub module: Option<Name>,
}

impl LibraryPackage {
    pub fn declares(self, db: &dyn crate::SourceDatabase, name: Name) -> bool {
        self.names(db).binary_search(&name).is_ok()
    }
}

/// Which libraries each source set compiles against, and the JDK.
#[salsa::input(debug)]
pub struct Classpaths {
    /// Indexed by `SourceSetId`.
    #[returns(ref)]
    pub by_source_set: Arc<[Arc<[Library]>]>,
    #[returns(copy)]
    pub jdk: Option<Library>,
}
