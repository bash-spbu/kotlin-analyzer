//! `textDocument/hover`: a declaration's signature, its documentation, and
//! where it lives.

use hir::{FileId, ItemContainer, ItemLocation, RootDatabase};
use syntax::{Language, SyntaxNode, TextRange};

use crate::goto_definition::name_at;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HoverResult {
    pub markdown: String,
    /// The name hovered.
    pub range: TextRange,
}

pub(crate) fn hover(db: &RootDatabase, file: FileId, offset: usize) -> Option<HoverResult> {
    let name = name_at(db, file, offset)?;
    let location = *name.declarations.first()?;
    let tree = hir::item_tree(db, location.container);
    let item = tree.item(location.item);
    let mut markdown = format!("```kotlin\n{}\n```", tree.signature(location.item));
    if let Some(documentation) = documentation(db, location) {
        markdown.push_str("\n\n---\n\n");
        markdown.push_str(&documentation);
    }
    let place = match item.parent {
        Some(parent) => {
            let parent = tree.item(parent).name;
            if tree.package.is_root() {
                parent.to_string()
            } else {
                format!("{}.{parent}", tree.package)
            }
        }
        None => tree.package.to_string(),
    };
    if !place.is_empty() {
        markdown.push_str(&format!("\n\n---\n\nin `{place}`"));
    }
    Some(HoverResult {
        markdown,
        range: name.range,
    })
}

/// The KDoc or Javadoc comment just before the declaration, cleaned up.
fn documentation(db: &RootDatabase, location: ItemLocation) -> Option<String> {
    match location.container {
        ItemContainer::File(file) => {
            let parse = hir::parse(db, file);
            let tree = hir::file_item_tree(db, file);
            let range = tree.item(location.item).range;
            doc_comment(parse, range)
        }
        ItemContainer::Library(_) => {
            let source = hir::library_declaration_source(db, location)?;
            let language = Language::of_path(std::path::Path::new(&source.entry))?;
            let parse = syntax::parse(source.text.clone(), language);
            doc_comment(&parse, source.full_range)
        }
    }
}

fn doc_comment(parse: &syntax::Parse, range: TextRange) -> Option<String> {
    let node = node_with_range(parse.root(), range)?;
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        let text = parse.text_of(sibling);
        let is_comment = sibling.kind().contains("comment");
        if is_comment && text.starts_with("/**") {
            return Some(clean(text));
        }
        // Only annotations may stand between a declaration and its doc.
        if !is_comment && sibling.kind() != "annotation" && sibling.kind() != "file_annotation" {
            return None;
        }
        previous = sibling.prev_sibling();
    }
    None
}

fn node_with_range(root: SyntaxNode<'_>, range: TextRange) -> Option<SyntaxNode<'_>> {
    let mut node = root.descendant_for_byte_range(range.start as usize, range.end as usize)?;
    while TextRange::of(node) != range {
        node = node.parent()?;
    }
    // The outermost node of that range: the declaration itself.
    while let Some(parent) = node.parent() {
        if TextRange::of(parent) != range {
            break;
        }
        node = parent;
    }
    Some(node)
}

/// `/** … */` without its delimiters and leading `*`s, with Javadoc's
/// inline tags as Markdown.
fn clean(comment: &str) -> String {
    let body = comment.trim_start_matches("/**").trim_end_matches("*/");
    let lines: Vec<&str> = body
        .lines()
        .map(|line| {
            let line = line.trim();
            line.strip_prefix('*').map(str::trim_start).unwrap_or(line)
        })
        .collect();
    let text = lines.join("\n").trim().to_owned();
    inline_tags(&text)
}

/// `{@code x}` / `{@literal x}` → `` `x` ``, `{@link A#b}` → `` `A.b` ``.
fn inline_tags(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{@") {
        output.push_str(&rest[..start]);
        let tag = &rest[start + 2..];
        let Some(end) = tag.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let (kind, argument) = tag[..end].split_once(' ').unwrap_or((&tag[..end], ""));
        let argument = argument.trim();
        match kind {
            "code" | "literal" => output.push_str(&format!("`{argument}`")),
            "link" | "linkplain" => {
                let target = argument.split_whitespace().next().unwrap_or("");
                output.push_str(&format!("`{}`", target.replace('#', ".")));
            }
            _ => output.push_str(argument),
        }
        rest = &tag[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_lose_their_frame_and_javadoc_tags_become_markdown() {
        let comment = "/**\n     * Returns {@code true} when {@link java.util.List#isEmpty} says so.\n     *\n     * @param list the list\n     */";
        assert_eq!(
            clean(comment),
            "Returns `true` when `java.util.List.isEmpty` says so.\n\n@param list the list"
        );
    }
}
