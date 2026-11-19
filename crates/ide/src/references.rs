//! `textDocument/references` and `textDocument/rename`: every place a name
//! means the same declaration (or local) — found by searching the files
//! that spell the name and asking, at each occurrence, what it means.

use hir::{FileId, ItemContainer, ItemLocation, Language, LocalKind, RootDatabase, SourceDatabase};
use rayon::prelude::*;
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
    Local {
        file: FileId,
        declared: TextRange,
        /// `it` and `field` aren't declared in the source: no declaration
        /// to list, nothing to rename; a local class's uses as a type
        /// aren't recorded: nothing to rename either.
        kind: LocalKind,
    },
    Declarations(Vec<ItemLocation>),
}

impl Target {
    fn at(db: &RootDatabase, file: FileId, offset: usize) -> Option<(Target, TextRange)> {
        let name = name_at(db, file, offset)?;
        let target = match name.target {
            NameTarget::Local { local, .. } => Target::Local {
                file,
                declared: local.name_range,
                kind: local.kind,
            },
            NameTarget::Declarations(declarations) if declarations.is_empty() => return None,
            NameTarget::Declarations(declarations) => Target::Declarations(declarations),
        };
        Some((target, name.range))
    }

    /// The target with the `expect` or `actual` declarations its
    /// declarations pair with: one declaration on every platform.
    fn with_counterparts(self, db: &RootDatabase) -> Target {
        self.expanded(|declaration| hir::expect_actual_family(db, declaration))
    }

    /// The target with its counterparts and every member its members
    /// override or are overridden by: what renaming one renames.
    fn with_overrides(self, db: &RootDatabase) -> Target {
        self.with_counterparts(db)
            .expanded(|declaration| hir::override_family(db, declaration))
    }

    fn expanded(self, family_of: impl Fn(ItemLocation) -> Vec<ItemLocation>) -> Target {
        match self {
            Target::Declarations(declarations) => {
                let mut family: Vec<ItemLocation> = Vec::new();
                for declaration in declarations {
                    for member in family_of(declaration) {
                        if !family.contains(&member) {
                            family.push(member);
                        }
                    }
                }
                Target::Declarations(family)
            }
            local => local,
        }
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
    let target = target.with_counterparts(db);
    let names = searched_names(db, file, &target, written_name(db, file, range));
    target_references(db, &target, &names, include_declaration)
}

/// The names the target is written by: the one at the cursor, and for a
/// Java getter both its own and its property's (`getTitle()`, `title`).
fn searched_names(
    db: &RootDatabase,
    file: FileId,
    target: &Target,
    written: String,
) -> Vec<String> {
    let mut names = vec![written];
    let (Target::Declarations(declarations), Some(source)) = (target, db.source_file(file)) else {
        return names;
    };
    let types = hir::Types::new(db, source);
    for declaration in declarations {
        let Some(property) = types.java_property_name(*declaration) else {
            continue;
        };
        let getter = hir::item_tree(db, declaration.container)
            .item(declaration.item)
            .name
            .to_string();
        for name in [getter, property] {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

/// An occurrence of a name in its file, and whether it writes the value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Highlight {
    pub range: TextRange,
    pub access: Access,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    /// The declaration, an assignment's target, `x++`.
    Write,
}

/// The occurrences in `file` of what the name at `offset` means.
pub(crate) fn highlights(db: &RootDatabase, file: FileId, offset: usize) -> Vec<Highlight> {
    let Some((target, range)) = Target::at(db, file, offset) else {
        return Vec::new();
    };
    let target = target.with_counterparts(db);
    let name = written_name(db, file, range);
    let declared: Vec<TextRange> = declaration_ranges(db, &target)
        .into_iter()
        .filter(|declaration| declaration.file == file)
        .map(|declaration| declaration.range)
        .collect();
    let mut ranges: Vec<TextRange> = occurrences_in(db, &target, &name, file)
        .into_iter()
        .map(|found| found.range)
        .chain(declared.iter().copied())
        .collect();
    ranges.sort();
    ranges.dedup();
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let parse = hir::parse(db, source);
    ranges
        .into_iter()
        .map(|range| {
            let written = declared.contains(&range) || is_written(&parse, range);
            Highlight {
                range,
                access: if written { Access::Write } else { Access::Read },
            }
        })
        .collect()
}

/// Whether the name at `range` is what an assignment (`x = …`, `this.x +=
/// …`) or an increment (`x++`, `--x`) writes.
fn is_written(parse: &syntax::Parse, range: TextRange) -> bool {
    let Some(mut node) = parse
        .root()
        .descendant_for_byte_range(range.start as usize, range.end as usize)
    else {
        return false;
    };
    while let Some(parent) = node.parent() {
        match parent.kind() {
            // `x` of `x.y = …` is read: only the last name is written.
            "navigation_expression" if syntax::named_children(parent).next() == Some(node) => {
                return false;
            }
            "navigation_suffix" | "navigation_expression" | "directly_assignable_expression" => {
                node = parent;
            }
            "assignment" => {
                return syntax::named_children(parent).next() == Some(node);
            }
            "postfix_expression" | "prefix_expression" => {
                return syntax::has_token(parent, "++") || syntax::has_token(parent, "--");
            }
            _ => return false,
        }
    }
    false
}

/// The name written at `range` of `file`, without backticks.
fn written_name(db: &RootDatabase, file: FileId, range: TextRange) -> String {
    db.source_file(file)
        .map(|source| {
            source.text(db)[range.start as usize..range.end as usize]
                .trim_matches('`')
                .to_owned()
        })
        .unwrap_or_default()
}

fn target_references(
    db: &RootDatabase,
    target: &Target,
    names: &[String],
    include_declaration: bool,
) -> Vec<FileRange> {
    let mut found: Vec<FileRange> = names
        .iter()
        .flat_map(|name| occurrences(db, target, name))
        .collect();
    let declarations = declaration_ranges(db, target);
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
    check_renameable(db, &target.with_overrides(db))?;
    Ok(range)
}

pub(crate) fn rename(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
    new_name: &str,
) -> Result<SourceChange, RenameError> {
    let (target, range) = Target::at(db, file, offset).ok_or(RenameError::NothingToRename)?;
    // A member is renamed with what it overrides and what overrides it:
    // they are one member to the code that calls them.
    let target = target.with_overrides(db);
    check_renameable(db, &target)?;
    let new_name = new_name.trim();
    if !is_valid_name(new_name) {
        return Err(RenameError::InvalidName(new_name.to_owned()));
    }
    let mut by_file: Vec<(FileId, Vec<TextEdit>)> = Vec::new();
    let name = written_name(db, file, range);
    for occurrence in target_references(db, &target, std::slice::from_ref(&name), true) {
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
        Target::Local { kind, .. } if !kind.is_written() || !kind.uses_are_all_recorded() => {
            Err(RenameError::NothingToRename)
        }
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
    syntax::names::is_plain_identifier(name)
}

/// Where the target is declared.
fn declaration_ranges(db: &RootDatabase, target: &Target) -> Vec<FileRange> {
    match target {
        Target::Local { kind, .. } if !kind.is_written() => Vec::new(),
        Target::Local { file, declared, .. } => vec![FileRange {
            file: *file,
            range: *declared,
        }],
        Target::Declarations(declarations) => declarations
            .iter()
            .filter_map(|declaration| {
                let file = declaration.file()?;
                let source_map = hir::file_item_source_map(db, file);
                Some(FileRange {
                    file: file.file_id(db),
                    range: source_map.ranges(declaration.item).name_range,
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
        // Only files that can name a declaration are searched: each
        // occurrence found is resolved, and resolving means inferring the
        // file — a common name in a large workspace is in hundreds.
        Target::Declarations(declarations) => db
            .all_files()
            .into_iter()
            .filter(|file| file.language(db) == Language::Kotlin)
            .filter(|file| {
                declarations
                    .iter()
                    .any(|declaration| hir::sees(db, *file, *declaration))
            })
            .filter(|file| file.text(db).contains(name))
            .map(|file| file.file_id(db))
            .collect(),
    };
    // Resolving an occurrence infers its file: the files are shared out
    // among threads, each with its own snapshot of the database.
    let per_thread = files
        .len()
        .div_ceil(rayon::current_num_threads().max(1))
        .max(1);
    let work: Vec<(RootDatabase, Vec<FileId>)> = files
        .chunks(per_thread)
        .map(|chunk| (db.clone(), chunk.to_vec()))
        .collect();
    work.into_par_iter()
        .flat_map(|(snapshot, chunk)| {
            chunk
                .into_iter()
                .flat_map(|file| occurrences_in(&snapshot, target, name, file))
                .collect::<Vec<FileRange>>()
        })
        .collect()
}

/// The occurrences of `name` in `file` that mean the target.
fn occurrences_in(db: &RootDatabase, target: &Target, name: &str, file: FileId) -> Vec<FileRange> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let text = source.text(db).clone();
    word_occurrences(&text, name)
        .into_iter()
        .filter_map(|start| Target::at(db, file, start))
        .filter(|(meaning, _)| target.includes(meaning))
        .map(|(_, range)| FileRange { file, range })
        .collect()
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
