//! `workspace/willRenameFiles`: what renaming or moving Kotlin files changes
//! in the code, as Kotlin's conventions tie names to files.
//!
//! - A file renamed in its directory renames the top-level class it's
//!   named after (`Order.kt` → `Invoice.kt` renames `class Order`), with
//!   every use.
//! - A file moved to a directory that implies another package — when its
//!   package was the one its old directory implied — moves to that
//!   package: its `package` line changes; other files' imports and fully
//!   qualified names of its declarations follow; a file that used them
//!   without an import (same package, or a star import) gets one; and the
//!   moved file gets imports of what it used from its old package.

use hir::{FileId, ItemKind, ItemLocation, RootDatabase, SourceDatabase};
use intern::Path;
use rustc_hash::{FxHashMap, FxHashSet};
use syntax::{Parse, SyntaxNode, TextRange, named_children};

use crate::diagnostics::TextEdit;
use crate::diagnostics::imports::ImportPlacement;
use crate::references::{self, SourceChange};

/// A Kotlin file being renamed or moved.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileMove {
    pub file: FileId,
    /// Its name without the extension, before and after.
    pub old_stem: String,
    pub new_stem: String,
    /// The package its new directory implies; `None` outside every source
    /// root.
    pub new_directory_package: Option<Path>,
}

pub(crate) fn will_rename_files(db: &RootDatabase, moves: &[FileMove]) -> SourceChange {
    let mut change = Edits::default();
    let packages = PackageMoves::of(db, moves);
    // The classes renamed after their file, by their new names: an import
    // of a moved one names it so.
    let mut renamed: FxHashMap<ItemLocation, String> = FxHashMap::default();
    for file_move in moves {
        if let Some((class, new_name)) = rename_class_after_file(db, file_move, &mut change) {
            renamed.insert(class, new_name);
        }
    }
    for (file, package_move) in &packages.moves {
        move_to_package(db, *file, package_move, &packages, &renamed, &mut change);
    }
    change.finish(db)
}

/// The edits being gathered: text edits, and imports to add, per file.
#[derive(Default)]
struct Edits {
    edits: FxHashMap<FileId, Vec<TextEdit>>,
    imports: FxHashMap<FileId, Vec<String>>,
}

impl Edits {
    fn edit(&mut self, file: FileId, edit: TextEdit) {
        let edits = self.edits.entry(file).or_default();
        if !edits.contains(&edit) {
            edits.push(edit);
        }
    }

    fn import(&mut self, file: FileId, qualified: String) {
        let imports = self.imports.entry(file).or_default();
        if !imports.contains(&qualified) {
            imports.push(qualified);
        }
    }

    fn merge(&mut self, change: SourceChange) {
        for (file, edits) in change.edits {
            for edit in edits {
                self.edit(file, edit);
            }
        }
    }

    /// The edits, each file's imports added as one edit where an import
    /// goes.
    fn finish(mut self, db: &RootDatabase) -> SourceChange {
        for (file, mut imports) in std::mem::take(&mut self.imports) {
            let Some(source) = db.source_file(file) else {
                continue;
            };
            imports.sort();
            let placement = ImportPlacement::of(&hir::parse(db, source));
            let mut edit = placement.edit(&imports[0]);
            let first_line = format!("import {}\n", syntax::names::escaped_path(&imports[0]));
            let more: String = imports[1..]
                .iter()
                .map(|import| format!("import {}\n", syntax::names::escaped_path(import)))
                .collect();
            edit.new_text = edit
                .new_text
                .replacen(&first_line, &format!("{first_line}{more}"), 1);
            self.edit(file, edit);
        }
        let mut edits: Vec<(FileId, Vec<TextEdit>)> = self.edits.into_iter().collect();
        for (_, file_edits) in &mut edits {
            file_edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        }
        edits.sort_by_key(|(file, _)| *file);
        SourceChange { edits }
    }
}

/// `Order.kt` renamed `Invoice.kt`: its top-level `Order` (the only one so
/// named) is renamed `Invoice`. The class renamed, with its new name.
fn rename_class_after_file(
    db: &RootDatabase,
    file_move: &FileMove,
    change: &mut Edits,
) -> Option<(ItemLocation, String)> {
    let renamed = file_move.old_stem != file_move.new_stem;
    if !renamed || !syntax::names::is_plain_identifier(&file_move.new_stem) {
        return None;
    }
    let source = db.source_file(file_move.file)?;
    let tree = hir::file_item_tree(db, source);
    let named_after_file: Vec<_> = tree
        .top_level
        .iter()
        .filter(|item| {
            let item = tree.item(**item);
            item.kind.is_classifier() && item.name.as_str() == file_move.old_stem
        })
        .collect();
    let [class] = named_after_file[..] else {
        return None;
    };
    let name_range = hir::file_item_source_map(db, source)
        .ranges(*class)
        .name_range;
    let renamed = references::rename(
        db,
        file_move.file,
        name_range.start as usize,
        &file_move.new_stem,
    )
    .ok()?;
    change.merge(renamed);
    Some((
        ItemLocation::in_file(source, *class),
        file_move.new_stem.clone(),
    ))
}

/// A file going from one package to another.
#[derive(Clone, Debug)]
struct PackageMove {
    from: Path,
    to: Path,
}

/// The files of a rename whose package follows their directory, and each
/// moved file's package after it.
struct PackageMoves {
    moves: Vec<(FileId, PackageMove)>,
}

impl PackageMoves {
    fn of(db: &RootDatabase, moves: &[FileMove]) -> PackageMoves {
        let moves = moves
            .iter()
            .filter_map(|file_move| {
                let source = db.source_file(file_move.file)?;
                if source.language(db) != hir::Language::Kotlin {
                    return None;
                }
                let declared = hir::file_item_tree(db, source).package.clone();
                let followed_directory = source.directory_package(db).as_ref() == Some(&declared);
                let to = file_move.new_directory_package.clone()?;
                let moves_package = followed_directory && to != declared;
                if !moves_package {
                    return None;
                }
                Some((file_move.file, PackageMove { from: declared, to }))
            })
            .collect();
        PackageMoves { moves }
    }

    /// The package `file` is in once the rename is done.
    fn package_after(&self, db: &RootDatabase, file: FileId) -> Option<Path> {
        if let Some((_, package_move)) = self.moves.iter().find(|(moved, _)| *moved == file) {
            return Some(package_move.to.clone());
        }
        let source = db.source_file(file)?;
        Some(hir::file_item_tree(db, source).package.clone())
    }

    fn moves(&self, file: FileId) -> bool {
        self.moves.iter().any(|(moved, _)| *moved == file)
    }
}

fn move_to_package(
    db: &RootDatabase,
    file: FileId,
    package_move: &PackageMove,
    packages: &PackageMoves,
    renamed: &FxHashMap<ItemLocation, String>,
    change: &mut Edits,
) {
    let Some(source) = db.source_file(file) else {
        return;
    };
    let parse = hir::parse(db, source);
    if let Some(edit) = package_line(&parse, &package_move.to) {
        change.edit(file, edit);
    }
    let tree = hir::file_item_tree(db, source);
    let moved: Vec<ItemLocation> = tree
        .top_level
        .iter()
        .filter(|item| {
            let item = tree.item(**item);
            item.visibility != hir::Visibility::Private
                && matches!(
                    item.kind,
                    ItemKind::Class(_)
                        | ItemKind::Function
                        | ItemKind::Property { .. }
                        | ItemKind::TypeAlias
                )
        })
        .map(|item| ItemLocation::in_file(source, *item))
        .collect();
    let mut follow = Follow {
        db,
        package_move,
        packages,
        change,
        parses: FxHashMap::default(),
    };
    for declaration in moved {
        let names = Names {
            old: tree.item(declaration.item).name.to_string(),
            new: renamed.get(&declaration).cloned(),
        };
        for occurrence in references::declaration_occurrences(db, declaration) {
            if occurrence.file != file {
                follow.occurrence(occurrence.file, occurrence.range, &names);
            }
        }
    }
    imports_of_old_package(db, file, &parse, package_move, packages, change);
}

/// The `package` line saying `package`: the old one's path replaced, or a
/// line added; none for the root package.
fn package_line(parse: &Parse, package: &Path) -> Option<TextEdit> {
    let header = named_children(parse.root()).find(|child| child.kind() == "package_header");
    match header {
        Some(header) if package.is_root() => Some(TextEdit {
            range: TextRange::of(header),
            new_text: String::new(),
        }),
        Some(header) => {
            let path = named_children(header).find(|child| child.kind() == "identifier")?;
            Some(TextEdit {
                range: TextRange::of(path),
                new_text: syntax::names::escaped_path(&package.to_string()),
            })
        }
        None if package.is_root() => None,
        None => Some(TextEdit {
            range: TextRange::new(0, 0),
            new_text: format!(
                "package {}\n\n",
                syntax::names::escaped_path(&package.to_string())
            ),
        }),
    }
}

/// A moved declaration's name, and the one it's renamed to (with its
/// file).
struct Names {
    old: String,
    new: Option<String>,
}

impl Names {
    fn after(&self) -> &str {
        self.new.as_deref().unwrap_or(&self.old)
    }
}

/// How the uses of a moved declaration follow it.
struct Follow<'change> {
    db: &'change RootDatabase,
    package_move: &'change PackageMove,
    packages: &'change PackageMoves,
    change: &'change mut Edits,
    parses: FxHashMap<FileId, std::sync::Arc<Parse>>,
}

impl Follow<'_> {
    /// A use of the moved declaration `name` at `range` of `file`: an
    /// import's or a qualified name's package changes; a plain use gets
    /// an import, unless `file` ends up in the new package or imports the
    /// name already.
    fn occurrence(&mut self, file: FileId, range: TextRange, names: &Names) {
        let Some(parse) = self.parse(file) else {
            return;
        };
        let text = parse.text();
        let old = self.package_move.from.to_string();
        let new = self.package_move.to.to_string();
        if let Some(import) = enclosing(&parse, range, "import_header") {
            let Some(path) = named_children(import).find(|child| child.kind() == "identifier")
            else {
                return;
            };
            if let Some(edit) =
                requalified(text, TextRange::of(path).start, range.start, &old, &new)
            {
                self.change.edit(file, edit);
            }
            return;
        }
        let qualified_start = (range.start as usize).checked_sub(old.len() + 1);
        let fully_qualified = !old.is_empty()
            && qualified_start
                .is_some_and(|start| text[start..range.start as usize] == format!("{old}."));
        if fully_qualified {
            let start = qualified_start.expect("checked") as u32;
            if let Some(edit) = requalified(text, start, range.start, &old, &new) {
                self.change.edit(file, edit);
            }
            return;
        }
        // Reached through a receiver (`order.Item`): not a top-level use.
        let after_dot = text[..range.start as usize].trim_end().ends_with('.');
        if after_dot {
            return;
        }
        let in_new_package =
            self.packages.package_after(self.db, file).as_ref() == Some(&self.package_move.to);
        let imported =
            imports_name(&parse, &names.old, &old) || imports_name(&parse, names.after(), &new);
        if !in_new_package && !imported {
            self.change.import(file, qualified(&new, names.after()));
        }
    }

    fn parse(&mut self, file: FileId) -> Option<std::sync::Arc<Parse>> {
        if let Some(parse) = self.parses.get(&file) {
            return Some(parse.clone());
        }
        let source = self.db.source_file(file)?;
        let parse = hir::parse(self.db, source);
        self.parses.insert(file, parse.clone());
        Some(parse)
    }
}

/// `old.Name` written from `start` to `name_start` as `new.Name`: the
/// qualifier replaced (`None` when it isn't `old`).
fn requalified(text: &str, start: u32, name_start: u32, old: &str, new: &str) -> Option<TextEdit> {
    let qualifier = &text[start as usize..name_start as usize];
    let written: String = qualifier
        .chars()
        .filter(|char| !char.is_whitespace())
        .collect();
    let without_backticks = written.replace('`', "");
    if without_backticks != format!("{old}.") {
        return None;
    }
    let new_text = if new.is_empty() {
        String::new()
    } else {
        format!("{}.", syntax::names::escaped_path(new))
    };
    Some(TextEdit {
        range: TextRange {
            start,
            end: name_start,
        },
        new_text,
    })
}

/// The moved file's uses of declarations its old package has in files
/// that stay: each gets an import, as the package no longer covers it.
fn imports_of_old_package(
    db: &RootDatabase,
    file: FileId,
    parse: &Parse,
    package_move: &PackageMove,
    packages: &PackageMoves,
    change: &mut Edits,
) {
    let Some(source) = db.source_file(file) else {
        return;
    };
    let text = parse.text();
    let mut used: Vec<(TextRange, ItemLocation)> = hir::type_references(db, source)
        .iter()
        .map(|reference| (reference.range, reference.declaration))
        .collect();
    let inference = hir::infer_file(db, source);
    used.extend(
        inference
            .names()
            .filter_map(|(start, resolved)| match resolved.reference {
                hir::Reference::Declaration(location) => {
                    let name = hir::item_tree(db, location.container)
                        .item(location.item)
                        .name;
                    let range = TextRange {
                        start,
                        end: start + name.as_str().len() as u32,
                    };
                    Some((range, location))
                }
                _ => None,
            }),
    );
    let old = package_move.from.to_string();
    let mut imported: FxHashSet<String> = FxHashSet::default();
    for (range, location) in used {
        let Some(declaring) = location.file() else {
            continue;
        };
        let declaring_id = declaring.file_id(db);
        let stays_behind = declaring_id != file && !packages.moves(declaring_id);
        let tree = hir::item_tree(db, location.container);
        let item = tree.item(location.item);
        let top_level_of_old = item.parent.is_none() && tree.package == package_move.from;
        // Written plainly: not after a `.`, nor in an import.
        let plain = !text[..range.start as usize].trim_end().ends_with('.')
            && enclosing(parse, range, "import_header").is_none();
        if !(stays_behind && top_level_of_old && plain) {
            continue;
        }
        let name = item.name.to_string();
        if imports_name(parse, &name, &old) || !imported.insert(name.clone()) {
            continue;
        }
        change.import(file, qualified(&old, &name));
    }
}

/// `package.name` (`name` in the root package).
fn qualified(package: &str, name: &str) -> String {
    if package.is_empty() {
        name.to_owned()
    } else {
        format!("{package}.{name}")
    }
}

/// Whether the file imports `package.name` (or aliases it).
fn imports_name(parse: &Parse, name: &str, package: &str) -> bool {
    let wanted = qualified(package, name);
    syntax::descendants(parse.root())
        .filter(|node| node.kind() == "import_header")
        .filter_map(|import| named_children(import).find(|child| child.kind() == "identifier"))
        .any(|path| {
            let written: String = parse
                .text_of(path)
                .chars()
                .filter(|char| !char.is_whitespace() && *char != '`')
                .collect();
            written == wanted
        })
}

/// The `kind` node around `range`, if any.
fn enclosing<'tree>(
    parse: &'tree Parse,
    range: TextRange,
    kind: &str,
) -> Option<SyntaxNode<'tree>> {
    let mut node = parse
        .root()
        .descendant_for_byte_range(range.start as usize, range.end as usize)?;
    loop {
        if node.kind() == kind {
            return Some(node);
        }
        node = node.parent()?;
    }
}
