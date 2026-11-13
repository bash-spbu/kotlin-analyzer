//! Virtual file system: every file the server knows gets a [`FileId`]; text
//! comes from disk or from an open editor buffer, and every change is
//! recorded so the database sees one consistent snapshot. Nothing above this
//! crate reads the disk for source text.
//!
//! Also home of [`LineIndex`]: byte offsets ↔ LSP positions (UTF-16 columns).

mod line_index;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::FxHashMap;

pub use line_index::{LineColumn, LineIndex};

/// A file the server knows, by an index into the [`Vfs`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct FileId(u32);

impl FileId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What happened to a file between two [`Vfs::take_changes`] calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The file appeared, or its text changed.
    Written(Arc<str>),
    /// The file is gone.
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub file: FileId,
    pub change: Change,
}

/// Paths interned to [`FileId`]s, their current text, and the changes not
/// yet handed to the database.
#[derive(Default)]
pub struct Vfs {
    paths: Vec<PathBuf>,
    ids: FxHashMap<PathBuf, FileId>,
    contents: Vec<Option<Arc<str>>>,
    pending: Vec<ChangedFile>,
}

impl Vfs {
    /// The id of `path`, interning it on first sight.
    pub fn intern(&mut self, path: &Path) -> FileId {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = FileId(self.paths.len() as u32);
        self.paths.push(path.to_path_buf());
        self.contents.push(None);
        self.ids.insert(path.to_path_buf(), id);
        id
    }

    pub fn file_id(&self, path: &Path) -> Option<FileId> {
        self.ids.get(path).copied()
    }

    pub fn path(&self, file: FileId) -> &Path {
        &self.paths[file.index()]
    }

    pub fn contents(&self, file: FileId) -> Option<&Arc<str>> {
        self.contents[file.index()].as_ref()
    }

    /// Sets `path`'s text (`None`: deleted). Records a change only when the
    /// text actually differs.
    pub fn set_contents(&mut self, path: &Path, text: Option<Arc<str>>) -> FileId {
        let file = self.intern(path);
        let slot = &mut self.contents[file.index()];
        let unchanged = match (&*slot, &text) {
            (Some(old), Some(new)) => old == new,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return file;
        }
        let change = match &text {
            Some(text) => Change::Written(text.clone()),
            None => Change::Deleted,
        };
        *slot = text;
        self.pending.push(ChangedFile { file, change });
        file
    }

    /// The changes since the last call, oldest first.
    pub fn take_changes(&mut self) -> Vec<ChangedFile> {
        std::mem::take(&mut self.pending)
    }

    /// Every file that currently has text.
    pub fn existing_files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.contents
            .iter()
            .enumerate()
            .filter(|(_, text)| text.is_some())
            .map(|(index, _)| FileId(index as u32))
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_is_recorded_only_when_the_text_differs() {
        let mut vfs = Vfs::default();
        let path = Path::new("/work/A.kt");
        let file = vfs.set_contents(path, Some(Arc::from("class A")));
        vfs.set_contents(path, Some(Arc::from("class A")));
        vfs.set_contents(path, None);
        let changes = vfs.take_changes();
        assert_eq!(
            changes,
            vec![
                ChangedFile {
                    file,
                    change: Change::Written(Arc::from("class A"))
                },
                ChangedFile {
                    file,
                    change: Change::Deleted
                },
            ]
        );
        assert!(vfs.take_changes().is_empty());
        assert_eq!(vfs.file_id(path), Some(file));
        assert_eq!(vfs.existing_files().count(), 0);
    }
}
