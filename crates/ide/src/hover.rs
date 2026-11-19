//! `textDocument/hover`: a declaration's signature, its documentation, and
//! where it lives.

use hir::{FileId, ItemContainer, ItemLocation, Local, LocalKind, RootDatabase};
use syntax::{Language, SyntaxNode, TextRange};

use crate::goto_definition::{NameTarget, name_at};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HoverResult {
    pub markdown: String,
    /// The name hovered.
    pub range: TextRange,
}

pub(crate) fn hover(db: &RootDatabase, file: FileId, offset: usize) -> Option<HoverResult> {
    let name = name_at(db, file, offset)?;
    let location = match &name.target {
        NameTarget::Local { local, .. } => {
            return Some(HoverResult {
                markdown: format!("```kotlin\n{}\n```", local_signature(db, local)),
                range: name.range,
            });
        }
        NameTarget::Declarations(declarations) => *declarations.first()?,
    };
    let tree = hir::item_tree(db, location.container);
    let item = tree.item(location.item);
    let mut markdown = signature_block(&tree.signature(location.item));
    if let Some(ty) = name.ty.as_ref().filter(|ty| !ty.is_unknown())
        && type_depends_on_use(&tree, location)
    {
        let label = match item.kind {
            hir::ItemKind::Property { .. } | hir::ItemKind::EnumEntry => "Type",
            _ => "Returns",
        };
        markdown.push_str(&format!(
            "\n\n{label} here: `{}`",
            hir::display_type(db, ty)
        ));
    }
    push_documentation_and_place(db, location, &mut markdown);
    Some(HoverResult {
        markdown,
        range: name.range,
    })
}

/// Whether the type a use of the declaration gets can differ from what the
/// declaration writes: it, or its class, is generic, or it writes no type.
fn type_depends_on_use(tree: &hir::ItemTree, location: ItemLocation) -> bool {
    let item = tree.item(location.item);
    let generic_class = item
        .parent
        .is_some_and(|parent| !tree.item(parent).type_parameters.is_empty());
    let writes_no_type = matches!(
        &item.signature,
        hir::Signature::Function {
            return_type: None,
            ..
        } | hir::Signature::Property { type_ref: None, .. }
    );
    !item.type_parameters.is_empty() || generic_class || writes_no_type
}

/// `val name: Type`, `it: Type`, `fun name(…): Type`.
fn local_signature(db: &RootDatabase, local: &Local) -> String {
    let ty = hir::display_type(db, &local.ty);
    match local.kind {
        LocalKind::Value => format!("val {}: {ty}", local.name),
        LocalKind::Variable => format!("var {}: {ty}", local.name),
        LocalKind::Parameter
        | LocalKind::PropertyParameter
        | LocalKind::LambdaParameter
        | LocalKind::ImplicitIt
        | LocalKind::BackingField => format!("{}: {ty}", local.name),
        LocalKind::Function => format!("fun {}: {ty}", local.name),
        LocalKind::Class => format!("class {}", local.name),
    }
}

/// A declaration as completion documents it: its whole signature, its
/// KDoc or Javadoc, where it's declared.
pub(crate) fn declaration_markdown(db: &RootDatabase, location: ItemLocation) -> String {
    let tree = hir::item_tree(db, location.container);
    let mut markdown = signature_block(&tree.signature(location.item));
    push_documentation_and_place(db, location, &mut markdown);
    markdown
}

/// Adds the declaration's documentation, then `in package.Class`.
fn push_documentation_and_place(db: &RootDatabase, location: ItemLocation, markdown: &mut String) {
    if let Some(documentation) = documentation(db, location) {
        markdown.push_str("\n\n---\n\n");
        markdown.push_str(&documentation);
    }
    let tree = hir::item_tree(db, location.container);
    let place = match tree.item(location.item).parent {
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
}

/// Signatures longer than this get one parameter per line.
const SIGNATURE_WIDTH: usize = 60;

/// The signature in a `kotlin` code block; a long one with several
/// parameters with a parameter per line, as Kotlin's style writes it:
///
/// ```text
/// fun addAuditLog(
///     operation: AuditLogOperation,
///     entityId: EntityPk?,
/// ): Unit
/// ```
fn signature_block(signature: &str) -> String {
    format!("```kotlin\n{}\n```", wrapped_signature(signature))
}

fn wrapped_signature(signature: &str) -> String {
    if signature.len() <= SIGNATURE_WIDTH {
        return signature.to_owned();
    }
    let Some(parameters) = parameter_list(signature) else {
        return signature.to_owned();
    };
    let inside = &signature[parameters.start + 1..parameters.end];
    let split = top_level_parts(inside);
    // One parameter stays on the line, however long.
    if split.len() < 2 {
        return signature.to_owned();
    }
    let mut wrapped = signature[..=parameters.start].to_owned();
    for parameter in split {
        wrapped.push_str(&format!("\n    {parameter},"));
    }
    wrapped.push('\n');
    wrapped.push_str(&signature[parameters.end..]);
    wrapped
}

/// The byte range of the signature's parameter list, from its `(` to its
/// `)`: the first parenthesis outside type parameters (`fun <T> f(…)`).
fn parameter_list(signature: &str) -> Option<std::ops::Range<usize>> {
    let mut angle_depth = 0usize;
    let mut previous = ' ';
    let mut open = None;
    let mut depth = 0usize;
    for (index, char) in signature.char_indices() {
        match char {
            '<' if open.is_none() => angle_depth += 1,
            '>' if open.is_none() && previous != '-' => angle_depth = angle_depth.saturating_sub(1),
            '(' if open.is_none() && angle_depth == 0 => {
                open = Some(index);
                depth = 1;
            }
            '(' if open.is_some() => depth += 1,
            ')' if open.is_some() => {
                depth -= 1;
                if depth == 0 {
                    return Some(open?..index);
                }
            }
            _ => {}
        }
        previous = char;
    }
    None
}

/// `text` split at its top-level commas — not those in brackets, type
/// arguments or strings — each part trimmed.
fn top_level_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut previous = ' ';
    let mut start = 0;
    for (index, char) in text.char_indices() {
        match char {
            '"' if previous != '\\' => in_string = !in_string,
            _ if in_string => {}
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if previous == '-' => {}
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
        previous = char;
    }
    parts.push(text[start..].trim());
    parts.retain(|part| !part.is_empty());
    parts
}

/// The KDoc or Javadoc comment just before the declaration, cleaned up.
pub(crate) fn documentation(db: &RootDatabase, location: ItemLocation) -> Option<String> {
    match location.container {
        ItemContainer::File(file) => {
            let parse = hir::parse(db, file);
            let range = hir::file_item_source_map(db, file)
                .ranges(location.item)
                .range;
            doc_comment(&parse, range)
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
    fn a_long_signature_gets_a_parameter_per_line_and_a_short_one_stays() {
        assert_eq!(wrapped_signature("fun total(): Long"), "fun total(): Long");
        assert_eq!(
            wrapped_signature(
                "fun <T : Comparable<T>> addAuditLog(operation: Map<String, T>, \
                 entityId: EntityPk? = null, label: String = \"a, (b)\", \
                 onDone: (Int, String) -> Unit = {}): Unit"
            ),
            "fun <T : Comparable<T>> addAuditLog(\n    \
             operation: Map<String, T>,\n    \
             entityId: EntityPk? = null,\n    \
             label: String = \"a, (b)\",\n    \
             onDone: (Int, String) -> Unit = {},\n\
             ): Unit"
        );
        let class = "class Money(val amount: Long, val currency: Currency, val scale: Int)";
        assert_eq!(
            wrapped_signature(class),
            "class Money(\n    val amount: Long,\n    val currency: Currency,\n    val scale: Int,\n)"
        );
        let one_parameter =
            "fun register(handler: (AuditLogOperation, AuditLogOperationResult) -> Unit): Unit";
        assert_eq!(wrapped_signature(one_parameter), one_parameter);
        let long_without_parameters =
            "val someVeryLongPropertyName: Map<String, List<SomeVeryLongTypeName>>";
        assert_eq!(
            wrapped_signature(long_without_parameters),
            long_without_parameters
        );
    }

    #[test]
    fn comments_lose_their_frame_and_javadoc_tags_become_markdown() {
        let comment = "/**\n     * Returns {@code true} when {@link java.util.List#isEmpty} says so.\n     *\n     * @param list the list\n     */";
        assert_eq!(
            clean(comment),
            "Returns `true` when `java.util.List.isEmpty` says so.\n\n@param list the list"
        );
    }
}
