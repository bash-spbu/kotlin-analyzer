//! `textDocument/inlayHint`: the inferred types of locals that don't write
//! one (`val total = sum(prices)` shows `: Long`).

use hir::{FileId, LocalKind, RootDatabase, SourceDatabase, Ty};
use syntax::{Language, SyntaxNode, TextRange, named_children};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InlayHint {
    /// Where the hint goes: right after the name.
    pub offset: u32,
    pub label: String,
}

pub(crate) fn inlay_hints(
    db: &RootDatabase,
    file: FileId,
    range: Option<TextRange>,
) -> Vec<InlayHint> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    if source.language(db) != Language::Kotlin {
        return Vec::new();
    }
    let parse = hir::parse(db, source);
    let inference = hir::infer_file(db, source);
    let mut hints: Vec<InlayHint> = inference
        .locals()
        .filter(|(_, local)| matches!(local.kind, LocalKind::Value | LocalKind::Variable))
        .filter(|(_, local)| range.is_none_or(|range| range.contains_range(local.name_range)))
        .filter(|(_, local)| !matches!(local.ty, Ty::Unknown))
        .filter_map(|(_, local)| {
            let name = parse.root().descendant_for_byte_range(
                local.name_range.start as usize,
                local.name_range.end as usize,
            )?;
            let label = hir::display_type(db, &local.ty);
            needs_hint(&parse, name, &label).then(|| InlayHint {
                offset: local.name_range.end,
                label: format!(": {label}"),
            })
        })
        .collect();
    hints.sort_by_key(|hint| hint.offset);
    hints
}

/// No hint where the type is written, or obvious from a constructor call
/// (`val box = Box(…)`).
fn needs_hint(parse: &syntax::Parse, name: SyntaxNode<'_>, type_text: &str) -> bool {
    let Some(variable) = name
        .parent()
        .filter(|parent| parent.kind() == "variable_declaration")
    else {
        return true;
    };
    let writes_type = named_children(variable).any(syntax::ast::kotlin::is_type);
    if writes_type {
        return false;
    }
    let Some(declaration) = variable
        .parent()
        .filter(|parent| parent.kind() == "property_declaration")
    else {
        return true;
    };
    let class_name = type_text.split(['<', '?', '!']).next().unwrap_or(type_text);
    let constructed = named_children(declaration)
        .filter(|child| child.kind() == "call_expression")
        .filter_map(|call| named_children(call).next())
        .any(|callee| callee.kind() == "simple_identifier" && parse.text_of(callee) == class_name);
    !constructed
}
