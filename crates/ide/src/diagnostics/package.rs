//! `missing-package`: a file that declares something without a `package`
//! line, in a directory that implies one.

use hir::{RootDatabase, SourceFile};
use syntax::{Parse, TextRange, named_children};

use super::{Check, Diagnostic, Fix, TextEdit};

pub(super) fn missing_package(
    db: &RootDatabase,
    file: SourceFile,
    parse: &Parse,
) -> Option<Diagnostic> {
    let expected = file.directory_package(db).as_ref()?;
    if expected.is_root() {
        return None;
    }
    let root = parse.root();
    let has_package = named_children(root).any(|child| child.kind() == "package_header");
    let declares_something = !hir::file_item_tree(db, file).top_level.is_empty();
    if has_package || !declares_something {
        return None;
    }
    let first_declaration = named_children(root).find(|child| {
        !child.kind().contains("comment")
            && child.kind() != "file_annotation"
            && child.kind() != "shebang_line"
    })?;
    let start = first_declaration.start_byte();
    let insert_at = named_children(root)
        .find(|child| matches!(child.kind(), "import_list" | "import_header"))
        .map(|imports| imports.start_byte())
        .unwrap_or(start);
    Some(Diagnostic {
        check: Check::MissingPackage,
        range: TextRange::new(start, start),
        message: format!("No `package` line; the directory implies `package {expected}`"),
        fixes: vec![Fix {
            label: format!("Add `package {expected}`"),
            edits: vec![TextEdit {
                range: TextRange::new(insert_at, insert_at),
                new_text: format!("package {expected}\n\n"),
            }],
        }],
    })
}
