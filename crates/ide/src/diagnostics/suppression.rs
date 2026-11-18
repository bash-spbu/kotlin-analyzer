//! `@Suppress("…")` on the file (`@file:`), a declaration or an
//! expression: the names it turns findings off with, in the code it covers.

use syntax::{Parse, SyntaxNode, TextRange, named_children};

use super::Diagnostic;

/// Every `@Suppress` of a file: what it covers and the names it gives.
pub(super) struct Suppressions {
    covered: Vec<(TextRange, Vec<String>)>,
}

impl Suppressions {
    /// Whether a `@Suppress` covering the diagnostic names its check.
    pub(super) fn suppress(&self, diagnostic: &Diagnostic) -> bool {
        let names = diagnostic.check.suppressed_by();
        self.covered.iter().any(|(range, suppressed)| {
            range.contains_range(diagnostic.range)
                && suppressed.iter().any(|name| names.contains(&name.as_str()))
        })
    }
}

pub(super) fn suppressions(parse: &Parse) -> Suppressions {
    let mut covered = Vec::new();
    for node in syntax::descendants(parse.root()) {
        let is_annotation = matches!(node.kind(), "annotation" | "file_annotation");
        if !is_annotation {
            continue;
        }
        for suppress in suppress_invocations(parse, node) {
            let names: Vec<String> = syntax::descendants(suppress)
                .filter(|part| part.kind() == "string_content")
                .map(|string| parse.text_of(string).to_owned())
                .collect();
            if let Some(range) = annotated_range(parse, node) {
                covered.push((range, names));
            }
        }
    }
    Suppressions { covered }
}

/// The `Suppress(…)` invocations an annotation holds (`@file:[A B]` holds
/// several).
fn suppress_invocations<'tree>(
    parse: &Parse,
    annotation: SyntaxNode<'tree>,
) -> Vec<SyntaxNode<'tree>> {
    let mut invocations = vec![annotation];
    invocations.extend(named_children(annotation));
    invocations
        .into_iter()
        .filter(|node| {
            named_children(*node)
                .find(|child| child.kind() == "user_type")
                .is_some_and(|user_type| {
                    let written = parse.text_of(user_type);
                    written == "Suppress" || written == "kotlin.Suppress"
                })
        })
        .collect()
}

/// What an annotation covers: the whole file for `@file:`, else the
/// declaration or expression it annotates.
fn annotated_range(parse: &Parse, annotation: SyntaxNode<'_>) -> Option<TextRange> {
    if annotation.kind() == "file_annotation" {
        return Some(TextRange::of(parse.root()));
    }
    let parent = annotation.parent()?;
    let annotated = match parent.kind() {
        "modifiers" | "parameter_modifiers" => parent.parent()?,
        _ => parent,
    };
    Some(TextRange::of(annotated))
}
