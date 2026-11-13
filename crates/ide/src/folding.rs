//! `textDocument/foldingRange`: blocks, bodies, lambdas, the imports and
//! comments that span several lines.

use hir::{FileId, RootDatabase, SourceDatabase};
use syntax::TextRange;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FoldKind {
    Region,
    Imports,
    Comment,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fold {
    pub range: TextRange,
    pub kind: FoldKind,
}

pub(crate) fn folding_ranges(db: &RootDatabase, file: FileId) -> Vec<Fold> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let parse = hir::parse(db, source);
    let text = parse.text();
    let spans_lines =
        |range: TextRange| text[range.start as usize..range.end as usize].contains('\n');
    let mut folds = Vec::new();
    for node in syntax::descendants(parse.root()) {
        let kind = match node.kind() {
            "class_body"
            | "enum_class_body"
            | "function_body"
            | "lambda_literal"
            | "when_expression"
            | "control_structure_body"
            | "statements"
            | "block"
            | "constructor_body"
            | "class_body_declarations"
            | "interface_body"
            | "enum_body"
            | "switch_block"
            | "array_initializer" => FoldKind::Region,
            "import_list" => FoldKind::Imports,
            "multiline_comment" | "block_comment" => FoldKind::Comment,
            _ => continue,
        };
        let range = TextRange::of(node);
        // A body and its statements cover the same lines: fold once.
        let same_line = |from: u32, to: u32| !text[from as usize..to as usize].contains('\n');
        let duplicate = folds.last().is_some_and(|last: &Fold| {
            last.range.contains_range(range)
                && same_line(last.range.start, range.start)
                && same_line(range.end, last.range.end)
        });
        if spans_lines(range) && !duplicate {
            folds.push(Fold { range, kind });
        }
    }
    // Consecutive Java imports aren't grouped by the grammar.
    let java_imports: Vec<TextRange> = syntax::descendants(parse.root())
        .filter(|node| node.kind() == "import_declaration")
        .map(TextRange::of)
        .collect();
    if let (Some(first), Some(last)) = (java_imports.first(), java_imports.last())
        && java_imports.len() > 1
    {
        folds.push(Fold {
            range: TextRange {
                start: first.start,
                end: last.end,
            },
            kind: FoldKind::Imports,
        });
    }
    folds
}
