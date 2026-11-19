//! `textDocument/inlayHint`: the inferred types of what doesn't write one —
//! locals (`val total = sum(prices)` shows `: Long`), a lambda's parameters
//! and its `it` (`{ it: Order -> …`), an expression-bodied function's
//! result (`fun total() = …` shows `: Long` after its parameters).

use hir::{FileId, ItemKind, LocalKind, RootDatabase, Signature, SourceDatabase};
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
    let in_range =
        |name_range: TextRange| range.is_none_or(|range| range.contains_range(name_range));
    let mut hints: Vec<InlayHint> = inference
        .locals()
        .filter(|(_, local)| {
            matches!(
                local.kind,
                LocalKind::Value
                    | LocalKind::Variable
                    | LocalKind::LambdaParameter
                    | LocalKind::ImplicitIt
            )
        })
        .filter(|(_, local)| in_range(local.name_range))
        .filter(|(_, local)| !local.ty.is_unknown())
        .filter_map(|(_, local)| {
            let label = hir::display_type(db, &local.ty);
            // `it` has no name written: its hint follows the lambda's `{`.
            if local.kind == LocalKind::ImplicitIt {
                return Some(InlayHint {
                    offset: local.name_range.end,
                    label: format!("it: {label}"),
                });
            }
            let name = parse.root().descendant_for_byte_range(
                local.name_range.start as usize,
                local.name_range.end as usize,
            )?;
            needs_hint(&parse, name, &label).then(|| InlayHint {
                offset: local.name_range.end,
                label: format!(": {label}"),
            })
        })
        .collect();
    hints.extend(result_hints(db, source, &parse, &in_range));
    hints.sort_by_key(|hint| hint.offset);
    hints
}

/// The result types of the file's expression-bodied functions that write
/// none, after their parameters.
fn result_hints(
    db: &RootDatabase,
    file: hir::SourceFile,
    parse: &syntax::Parse,
    in_range: &impl Fn(TextRange) -> bool,
) -> Vec<InlayHint> {
    let tree = hir::file_item_tree(db, file);
    let source_map = hir::file_item_source_map(db, file);
    tree.indexed_items()
        .filter(|(_, item)| {
            matches!(
                (&item.kind, &item.signature),
                (
                    ItemKind::Function,
                    Signature::Function {
                        return_type: None,
                        ..
                    }
                )
            )
        })
        .filter_map(|(id, _)| {
            let ranges = source_map.ranges(id);
            if !in_range(ranges.name_range) {
                return None;
            }
            let function = parse.root().descendant_for_byte_range(
                ranges.range.start as usize,
                ranges.range.end as usize,
            )?;
            let body = named_children(function).find(|child| child.kind() == "function_body")?;
            let expression_body = syntax::has_token(body, "=");
            let parameters = named_children(function)
                .find(|child| child.kind() == "function_value_parameters")?;
            let ty = hir::declaration_type(db, file, id);
            (expression_body && !ty.is_unknown()).then(|| InlayHint {
                offset: parameters.end_byte() as u32,
                label: format!(": {}", hir::display_type(db, &ty)),
            })
        })
        .collect()
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
