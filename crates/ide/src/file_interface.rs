//! What other files' analysis reads of a file: its declarations and their
//! signatures, and the text of each declaration whose type is inferred from
//! it. Two versions of a file with the same interface leave every other
//! file's diagnostics as they were (`workspace/diagnostic` reports them
//! "unchanged").

use std::sync::Arc;

use hir::{FileId, ItemTree, RootDatabase, Signature, SourceDatabase};
use syntax::{Parse, TextRange};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileInterface {
    /// The declarations, positions left out (see `ItemTree`).
    declarations: Arc<ItemTree>,
    /// Without whitespace, the text of each function written without a
    /// return type and with an expression body, and of each property
    /// written without a type: their types are inferred from it.
    inferred_from: Vec<String>,
}

pub(crate) fn file_interface(db: &RootDatabase, file: FileId) -> Option<FileInterface> {
    let source = db.source_file(file)?;
    let declarations = hir::file_item_tree(db, source);
    let source_map = hir::file_item_source_map(db, source);
    let parse = hir::parse(db, source);
    let inferred_from = declarations
        .indexed_items()
        .filter_map(|(item_id, item)| {
            let range = source_map.ranges(item_id).range;
            let inferred = match &item.signature {
                Signature::Function {
                    return_type: None, ..
                } => has_expression_body(&parse, range),
                Signature::Property { type_ref: None, .. } => true,
                _ => false,
            };
            inferred.then(|| without_whitespace(parse.text().get(range.as_range()).unwrap_or("")))
        })
        .collect();
    Some(FileInterface {
        declarations,
        inferred_from,
    })
}

/// Whether the function declared at `range` has an expression body
/// (`= …`): a block body without a return type is `Unit`'s.
fn has_expression_body(parse: &Parse, range: TextRange) -> bool {
    let root = parse.root();
    let Some(mut node) = root.descendant_for_byte_range(range.start as usize, range.end as usize)
    else {
        return true;
    };
    while node.kind() != "function_declaration" {
        let Some(parent) = node.parent() else {
            // Not found as expected: count it, a body more than needed.
            return true;
        };
        node = parent;
    }
    syntax::named_children(node)
        .find(|child| child.kind() == "function_body")
        .is_some_and(|body| syntax::has_token(body, "="))
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|char| !char.is_whitespace()).collect()
}
