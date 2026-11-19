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
    let mut markdown = declaration_block(db, location);
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
    let mut markdown = declaration_block(db, location);
    push_documentation_and_place(db, location, &mut markdown);
    markdown
}

/// The declaration's signature in a code block: a Java one as Java,
/// with the parameter names its source writes; a Kotlin one with its
/// default values.
fn declaration_block(db: &RootDatabase, location: ItemLocation) -> String {
    if is_java(db, location) {
        let names = with_declaration_node(db, location, java_parameter_names);
        let signature =
            hir::item_tree(db, location.container).java_signature(location.item, names.as_deref());
        return signature_block(&signature, CodeLanguage::Java);
    }
    signature_block(&kotlin_signature(db, location), CodeLanguage::Kotlin)
}

/// The declaration's signature with the default values its source writes
/// (`currency: String = "EUR"`); `…` for one too long to show, and for all
/// of a compiled library's without sources.
fn kotlin_signature(db: &RootDatabase, location: ItemLocation) -> String {
    let signature = hir::item_tree(db, location.container).signature(location.item);
    let Some(defaults) = with_declaration_node(db, location, written_defaults) else {
        return signature;
    };
    let placeholders = signature.matches(DEFAULT_PLACEHOLDER).count();
    if placeholders != defaults.len() {
        return signature;
    }
    let mut parts = signature.split(DEFAULT_PLACEHOLDER);
    let mut written = parts.next().unwrap_or_default().to_owned();
    for (default, rest) in defaults.iter().zip(parts) {
        written.push_str(" = ");
        written.push_str(default);
        written.push_str(rest);
    }
    written
}

/// Declared in Java: a workspace `.java` file, or a library's class
/// compiled from Java.
fn is_java(db: &RootDatabase, location: ItemLocation) -> bool {
    match location.container {
        ItemContainer::File(file) => hir::parse(db, file).language() == Language::Java,
        ItemContainer::Library(_) => hir::item_tree(db, location.container)
            .item(location.item)
            .modifiers
            .contains(hir::Modifiers::PLATFORM),
    }
}

/// The parameter names a Java method or constructor's source writes —
/// what a class file may not keep.
fn java_parameter_names(parse: &syntax::Parse, declaration: SyntaxNode<'_>) -> Option<Vec<String>> {
    let parameters = declaration.child_by_field_name("parameters")?;
    let names = syntax::named_children(parameters)
        .filter_map(|parameter| match parameter.kind() {
            "formal_parameter" => parameter.child_by_field_name("name"),
            "spread_parameter" => syntax::named_children(parameter)
                .find(|child| child.kind() == "variable_declarator")
                .and_then(|declarator| declarator.child_by_field_name("name")),
            _ => None,
        })
        .map(|name| parse.text_of(name).to_owned())
        .collect();
    Some(names)
}

/// How a signature shows that a parameter has a default value.
const DEFAULT_PLACEHOLDER: &str = " = …";

/// A default value longer than this is shown as `…`.
const DEFAULT_WIDTH: usize = 40;

/// The default values of the declaration's parameters, in order, each on
/// one line: `fun f(a: Int = 1)`'s parameters, or a class's primary
/// constructor's.
fn written_defaults(parse: &syntax::Parse, declaration: SyntaxNode<'_>) -> Option<Vec<String>> {
    let parameters = syntax::named_children(declaration).find(|child| {
        matches!(
            child.kind(),
            "function_value_parameters" | "primary_constructor"
        )
    })?;
    // A function's default follows its parameter; a class parameter holds
    // its own.
    let holders: Vec<SyntaxNode<'_>> = if parameters.kind() == "primary_constructor" {
        syntax::named_children(parameters)
            .filter(|child| child.kind() == "class_parameter")
            .collect()
    } else {
        vec![parameters]
    };
    let mut defaults = Vec::new();
    for holder in holders {
        let mut after_equals = false;
        for child in syntax::children(holder) {
            if child.kind() == "=" {
                after_equals = true;
            } else if after_equals && child.is_named() {
                let one_line = on_one_line(parse.text_of(child));
                let shown = if one_line.chars().count() > DEFAULT_WIDTH {
                    "…".to_owned()
                } else {
                    one_line
                };
                defaults.push(shown);
                after_equals = false;
            }
        }
    }
    Some(defaults)
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
fn signature_block(signature: &str, language: CodeLanguage) -> String {
    format!(
        "```{}\n{}\n```",
        language.fence_name(),
        wrapped_signature(signature, language)
    )
}

/// The language a signature is written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CodeLanguage {
    Kotlin,
    Java,
}

impl CodeLanguage {
    fn fence_name(self) -> &'static str {
        match self {
            CodeLanguage::Kotlin => "kotlin",
            CodeLanguage::Java => "java",
        }
    }

    /// Kotlin's style ends a parameter list a parameter per line with a
    /// comma; Java doesn't allow one.
    fn trailing_comma(self) -> bool {
        self == CodeLanguage::Kotlin
    }
}

fn wrapped_signature(signature: &str, language: CodeLanguage) -> String {
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
    let last = split.len() - 1;
    for (index, parameter) in split.iter().enumerate() {
        let comma = index < last || language.trailing_comma();
        wrapped.push_str(&format!(
            "\n    {parameter}{}",
            if comma { "," } else { "" }
        ));
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

/// An expression written over several lines on one: its line breaks and
/// indents a space, none just inside brackets, no trailing comma
/// (`listOf(\n    "a",\n)` is `listOf("a")`).
fn on_one_line(expression: &str) -> String {
    let joined = expression.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut one_line = joined.clone();
    for (spaced, tight) in [
        (", )", ")"),
        (",)", ")"),
        (", ]", "]"),
        ("( ", "("),
        ("[ ", "["),
        (" )", ")"),
        (" ]", "]"),
    ] {
        one_line = one_line.replace(spaced, tight);
    }
    // A string's own spaces stay: only an expression without one is tidied.
    if expression.contains('"') && one_line != joined {
        let in_string_changed = joined.split('"').skip(1).step_by(2).collect::<String>()
            != one_line.split('"').skip(1).step_by(2).collect::<String>();
        if in_string_changed {
            return joined;
        }
    }
    one_line
}

/// The KDoc or Javadoc comment just before the declaration, cleaned up.
pub(crate) fn documentation(db: &RootDatabase, location: ItemLocation) -> Option<String> {
    with_declaration_node(db, location, doc_comment)
}

/// `read` of the declaration's syntax node: in its workspace file, or in
/// its library's sources — not in a file rendered from class files, which
/// says nothing they don't.
fn with_declaration_node<T>(
    db: &RootDatabase,
    location: ItemLocation,
    read: impl FnOnce(&syntax::Parse, SyntaxNode<'_>) -> Option<T>,
) -> Option<T> {
    let (parse, range) = match location.container {
        ItemContainer::File(file) => {
            let range = hir::file_item_source_map(db, file)
                .ranges(location.item)
                .range;
            (hir::parse(db, file), range)
        }
        ItemContainer::Library(_) => {
            let source = hir::library_declaration_source(db, location)
                .filter(|source| source.origin == hir::SourceOrigin::Archive)?;
            let language = Language::of_path(std::path::Path::new(&source.entry))?;
            let parse = syntax::parse(source.text.clone(), language);
            (std::sync::Arc::new(parse), source.full_range)
        }
    };
    let node = node_with_range(parse.root(), range)?;
    read(&parse, node)
}

fn doc_comment(parse: &syntax::Parse, node: SyntaxNode<'_>) -> Option<String> {
    let mut previous = node.prev_sibling();
    while let Some(sibling) = previous {
        let text = parse.text_of(sibling);
        let is_comment = sibling.kind().contains("comment");
        if is_comment && text.starts_with("/**") {
            let markdown = crate::doc_markdown::to_markdown(text, parse.language());
            return (!markdown.is_empty()).then_some(markdown);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_written_over_lines_is_tidied_onto_one() {
        assert_eq!(
            on_one_line("listOf(\n    \"audit\",\n    \"money\",\n)"),
            "listOf(\"audit\", \"money\")"
        );
        assert_eq!(on_one_line("mapOf(\n  1 to 2 )"), "mapOf(1 to 2)");
        // Spaces in a string are its own.
        assert_eq!(
            on_one_line("format(\"( x )\",\n 1)"),
            "format(\"( x )\", 1)"
        );
    }

    #[test]
    fn a_long_signature_gets_a_parameter_per_line_and_a_short_one_stays() {
        assert_eq!(
            wrapped_signature("fun total(): Long", CodeLanguage::Kotlin),
            "fun total(): Long"
        );
        assert_eq!(
            wrapped_signature(
                "fun <T : Comparable<T>> addAuditLog(operation: Map<String, T>, \
                 entityId: EntityPk? = null, label: String = \"a, (b)\", \
                 onDone: (Int, String) -> Unit = {}): Unit",
                CodeLanguage::Kotlin,
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
            wrapped_signature(class, CodeLanguage::Kotlin),
            "class Money(\n    val amount: Long,\n    val currency: Currency,\n    val scale: Int,\n)"
        );
        let one_parameter =
            "fun register(handler: (AuditLogOperation, AuditLogOperationResult) -> Unit): Unit";
        assert_eq!(
            wrapped_signature(one_parameter, CodeLanguage::Kotlin),
            one_parameter
        );
        // Java has no trailing comma.
        assert_eq!(
            wrapped_signature(
                "void success(long durationMs, String operation, String tenant, String region)",
                CodeLanguage::Java,
            ),
            "void success(\n    long durationMs,\n    String operation,\n    String tenant,\n    String region\n)"
        );
        let long_without_parameters =
            "val someVeryLongPropertyName: Map<String, List<SomeVeryLongTypeName>>";
        assert_eq!(
            wrapped_signature(long_without_parameters, CodeLanguage::Kotlin),
            long_without_parameters
        );
    }
}
