//! `textDocument/inlayHint`: the inferred types of what doesn't write one —
//! locals (`val total = sum(prices)` shows `: Long`), a lambda's parameters
//! and its `it` (`{ it: Order -> …`), an expression-bodied function's
//! result (`fun total() = …` shows `: Long` after its parameters). Each
//! class name in a hint links to its declaration.

use hir::{FileId, ItemKind, LocalKind, RootDatabase, Signature, SourceDatabase, Ty};
use syntax::{Language, SyntaxNode, TextRange, named_children};

use crate::NavigationTarget;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InlayHint {
    /// Where the hint goes: right after the name.
    pub offset: u32,
    /// The hint's text in pieces: each class name apart, linked.
    pub label: Vec<InlayHintPart>,
}

impl InlayHint {
    pub fn text(&self) -> String {
        self.label.iter().map(|part| part.text.as_str()).collect()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InlayHintPart {
    pub text: String,
    pub target: Option<HintTarget>,
    /// The class the part names.
    class: Option<hir::ItemLocation>,
}

/// Where a class name in a hint leads.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HintTarget {
    /// A workspace class: its declaration.
    Declaration(NavigationTarget),
    /// A library class: its declaration is read from the library's sources
    /// only when asked for (`resolve_inlay_hint`).
    LibraryDeclaration,
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
            // `it` has no name written: its hint follows the lambda's `{`.
            if local.kind == LocalKind::ImplicitIt {
                return Some(InlayHint {
                    offset: local.name_range.end,
                    label: label(db, "it: ", &local.ty),
                });
            }
            let type_text = hir::display_type(db, &local.ty);
            let name = parse.root().descendant_for_byte_range(
                local.name_range.start as usize,
                local.name_range.end as usize,
            )?;
            needs_hint(&parse, name, &type_text).then(|| InlayHint {
                offset: local.name_range.end,
                label: label(db, ": ", &local.ty),
            })
        })
        .collect();
    hints.extend(result_hints(db, source, &parse, &in_range));
    hints.sort_by_key(|hint| hint.offset);
    hints
}

/// The declarations the class names of the hint at `offset` lead to, part
/// by part (`None` for a part that names no class, or one whose source
/// isn't available) — the library ones too.
pub(crate) fn resolve_inlay_hint(
    db: &RootDatabase,
    file: FileId,
    offset: u32,
) -> Vec<Option<NavigationTarget>> {
    let Some(hint) = inlay_hints(db, file, None)
        .into_iter()
        .find(|hint| hint.offset == offset)
    else {
        return Vec::new();
    };
    hint.label
        .iter()
        .map(|part| part.class.and_then(|class| NavigationTarget::of(db, class)))
        .collect()
}

/// `prefix` and `ty`, a class name linked to a workspace declaration
/// directly, to a library's when resolved.
fn label(db: &RootDatabase, prefix: &str, ty: &Ty) -> Vec<InlayHintPart> {
    let mut label = vec![InlayHintPart {
        text: prefix.to_owned(),
        target: None,
        class: None,
    }];
    for part in hir::display_type_parts(db, ty) {
        let target = part.class.map(|class| match class.file() {
            Some(_) => NavigationTarget::of(db, class)
                .map_or(HintTarget::LibraryDeclaration, HintTarget::Declaration),
            None => HintTarget::LibraryDeclaration,
        });
        match (target, label.last_mut()) {
            (None, Some(last)) if last.target.is_none() => last.text.push_str(&part.text),
            (target, _) => label.push(InlayHintPart {
                text: part.text,
                target,
                class: part.class,
            }),
        }
    }
    label
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
                label: label(db, ": ", &ty),
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
