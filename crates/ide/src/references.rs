//! `textDocument/references` and `textDocument/rename`: every place a name
//! means the same declaration (or local) — found by searching the files
//! that spell the name and asking, at each occurrence, what it means.

use hir::{FileId, ItemContainer, ItemLocation, Language, RootDatabase, SourceDatabase};
use syntax::TextRange;

use crate::diagnostics::TextEdit;
use crate::goto_definition::{NameTarget, name_at};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct FileRange {
    pub file: FileId,
    pub range: TextRange,
}

/// Edits across files.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SourceChange {
    pub edits: Vec<(FileId, Vec<TextEdit>)>,
}

/// Why a rename can't happen.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RenameError {
    NothingToRename,
    /// A library's or a Java declaration: not ours to change.
    ReadOnly,
    InvalidName(String),
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameError::NothingToRename => formatter.write_str("No symbol to rename here"),
            RenameError::ReadOnly => {
                formatter.write_str("Only Kotlin declarations in the workspace can be renamed")
            }
            RenameError::InvalidName(name) => {
                write!(formatter, "`{name}` isn't a valid Kotlin name")
            }
        }
    }
}

/// What the name at a position refers to, as something to compare others
/// with.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Target {
    Local { file: FileId, declared: TextRange },
    Declarations(Vec<ItemLocation>),
}

impl Target {
    fn at(db: &RootDatabase, file: FileId, offset: usize) -> Option<(Target, TextRange)> {
        let name = name_at(db, file, offset)?;
        let target = match name.target {
            NameTarget::Local { local, .. } => Target::Local {
                file,
                declared: local.name_range,
            },
            NameTarget::Declarations(declarations) if declarations.is_empty() => return None,
            NameTarget::Declarations(declarations) => Target::Declarations(declarations),
        };
        Some((target, name.range))
    }

    /// Whether an occurrence meaning `other` refers to this target.
    fn includes(&self, other: &Target) -> bool {
        match (self, other) {
            (Target::Local { .. }, Target::Local { .. }) => self == other,
            (Target::Declarations(ours), Target::Declarations(theirs)) => {
                theirs.iter().any(|declaration| ours.contains(declaration))
            }
            _ => false,
        }
    }
}

pub(crate) fn references(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
    include_declaration: bool,
) -> Vec<FileRange> {
    let Some((target, range)) = Target::at(db, file, offset) else {
        return Vec::new();
    };
    let name = db
        .source_file(file)
        .map(|source| {
            source.text(db)[range.start as usize..range.end as usize]
                .trim_matches('`')
                .to_owned()
        })
        .unwrap_or_default();
    let mut found = occurrences(db, &target, &name);
    let declarations = declaration_ranges(db, &target);
    if include_declaration {
        found.extend(declarations.iter().copied());
    } else {
        found.retain(|occurrence| !declarations.contains(occurrence));
    }
    found.sort();
    found.dedup();
    found
}

/// Can the name at `offset` be renamed? Its range when it can.
pub(crate) fn prepare_rename(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Result<TextRange, RenameError> {
    let (target, range) = Target::at(db, file, offset).ok_or(RenameError::NothingToRename)?;
    check_renameable(db, &target)?;
    Ok(range)
}

pub(crate) fn rename(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
    new_name: &str,
) -> Result<SourceChange, RenameError> {
    let (target, _) = Target::at(db, file, offset).ok_or(RenameError::NothingToRename)?;
    check_renameable(db, &target)?;
    let new_name = new_name.trim();
    if !is_valid_name(new_name) {
        return Err(RenameError::InvalidName(new_name.to_owned()));
    }
    let mut by_file: Vec<(FileId, Vec<TextEdit>)> = Vec::new();
    for occurrence in references(db, file, offset, true) {
        let edit = TextEdit {
            range: occurrence.range,
            new_text: new_name.to_owned(),
        };
        match by_file
            .iter_mut()
            .find(|(file, _)| *file == occurrence.file)
        {
            Some((_, edits)) => edits.push(edit),
            None => by_file.push((occurrence.file, vec![edit])),
        }
    }
    Ok(SourceChange { edits: by_file })
}

fn check_renameable(db: &RootDatabase, target: &Target) -> Result<(), RenameError> {
    match target {
        Target::Local { .. } => Ok(()),
        Target::Declarations(declarations) => {
            let all_kotlin_sources =
                declarations
                    .iter()
                    .all(|declaration| match declaration.container {
                        ItemContainer::File(file) => file.language(db) == Language::Kotlin,
                        ItemContainer::Library(_) => false,
                    });
            if all_kotlin_sources {
                Ok(())
            } else {
                Err(RenameError::ReadOnly)
            }
        }
    }
}

/// A plain Kotlin identifier (a keyword would need backticks: refused).
fn is_valid_name(name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "as",
        "break",
        "class",
        "continue",
        "do",
        "else",
        "false",
        "for",
        "fun",
        "if",
        "in",
        "interface",
        "is",
        "null",
        "object",
        "package",
        "return",
        "super",
        "this",
        "throw",
        "true",
        "try",
        "typealias",
        "typeof",
        "val",
        "var",
        "when",
        "while",
    ];
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_');
    starts_well
        && chars.all(|char| char.is_alphanumeric() || char == '_')
        && !KEYWORDS.contains(&name)
}

/// Where the target is declared.
fn declaration_ranges(db: &RootDatabase, target: &Target) -> Vec<FileRange> {
    match target {
        Target::Local { file, declared } => vec![FileRange {
            file: *file,
            range: *declared,
        }],
        Target::Declarations(declarations) => declarations
            .iter()
            .filter_map(|declaration| {
                let file = declaration.file()?;
                let tree = hir::file_item_tree(db, file);
                Some(FileRange {
                    file: file.file_id(db),
                    range: tree.item(declaration.item).name_range,
                })
            })
            .collect(),
    }
}

/// Every occurrence of `name` that means the target: in its file for a
/// local, in every Kotlin file spelling the name otherwise.
fn occurrences(db: &RootDatabase, target: &Target, name: &str) -> Vec<FileRange> {
    let files: Vec<FileId> = match target {
        Target::Local { file, .. } => vec![*file],
        Target::Declarations(_) => db
            .all_files()
            .into_iter()
            .filter(|file| file.language(db) == Language::Kotlin && file.text(db).contains(name))
            .map(|file| file.file_id(db))
            .collect(),
    };
    let mut found = Vec::new();
    for file in files {
        let Some(source) = db.source_file(file) else {
            continue;
        };
        let text = source.text(db).clone();
        for start in word_occurrences(&text, name) {
            let Some((meaning, range)) = Target::at(db, file, start) else {
                continue;
            };
            if target.includes(&meaning) {
                found.push(FileRange { file, range });
            }
        }
    }
    found
}

/// The starts of `name` in `text` as a whole word (backticked or not).
fn word_occurrences(text: &str, name: &str) -> Vec<usize> {
    let is_word = |char: char| char.is_alphanumeric() || char == '_';
    text.match_indices(name)
        .map(|(start, _)| start)
        .filter(|start| {
            let before = text[..*start].chars().next_back();
            let after = text[start + name.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .collect()
}
