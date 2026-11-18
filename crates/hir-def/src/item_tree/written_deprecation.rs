//! `@Deprecated(message, replaceWith = ReplaceWith(expression), level)` as
//! written on a Kotlin declaration.

use syntax::{Parse, SyntaxNode, has_token, named_children};

use super::{Deprecation, DeprecationLevel, DeprecationOrigin};

/// The deprecation the annotations of `declaration` (a declaration or a
/// class parameter) write, if any. `@java.lang.Deprecated` is Java's: it
/// doesn't pass to overrides.
pub(super) fn written_deprecation(
    parse: &Parse,
    declaration: SyntaxNode<'_>,
) -> Option<Deprecation> {
    let modifiers = named_children(declaration).find(|child| child.kind() == "modifiers")?;
    named_children(modifiers)
        .filter(|child| child.kind() == "annotation")
        .find_map(|annotation| annotation_deprecation(parse, annotation))
}

fn annotation_deprecation(parse: &Parse, annotation: SyntaxNode<'_>) -> Option<Deprecation> {
    let invocation =
        named_children(annotation).find(|child| child.kind() == "constructor_invocation");
    let user_type = match invocation {
        Some(invocation) => named_children(invocation).find(|child| child.kind() == "user_type")?,
        None => named_children(annotation).find(|child| child.kind() == "user_type")?,
    };
    let origin = match parse.text_of(user_type) {
        "Deprecated" | "kotlin.Deprecated" => DeprecationOrigin::Kotlin,
        "java.lang.Deprecated" => DeprecationOrigin::Java,
        _ => return None,
    };
    let mut deprecation = Deprecation {
        level: DeprecationLevel::Warning,
        message: None,
        replace_with: None,
        origin,
        since_kotlin: None,
    };
    let arguments = invocation.and_then(|invocation| {
        named_children(invocation).find(|child| child.kind() == "value_arguments")
    });
    let Some(arguments) = arguments else {
        return Some(deprecation);
    };
    // Kotlin's parameters, in order: `message`, `replaceWith`, `level`.
    const POSITIONS: [&str; 3] = ["message", "replaceWith", "level"];
    let value_arguments =
        named_children(arguments).filter(|child| child.kind() == "value_argument");
    for (position, argument) in value_arguments.enumerate() {
        let parts: Vec<SyntaxNode<'_>> = named_children(argument).collect();
        let is_named = has_token(argument, "=") && parts.len() >= 2;
        let parameter = match is_named {
            true => parse.text_of(parts[0]),
            false => POSITIONS.get(position).copied().unwrap_or(""),
        };
        let Some(&value) = parts.last() else {
            continue;
        };
        match parameter {
            "message" => deprecation.message = plain_string(parse, value),
            "replaceWith" => deprecation.replace_with = replacement(parse, value),
            "level" => deprecation.level = level(parse.text_of(value)),
            _ => {}
        }
    }
    Some(deprecation)
}

/// A string literal without templates, as its text; anything else (a
/// concatenation, a constant) isn't followed.
fn plain_string(parse: &Parse, value: SyntaxNode<'_>) -> Option<Box<str>> {
    if value.kind() != "string_literal" {
        return None;
    }
    let only_text = named_children(value).all(|part| part.kind() == "string_content");
    if !only_text {
        return None;
    }
    let text: String = named_children(value)
        .map(|part| parse.text_of(part))
        .collect();
    Some(text.into())
}

/// `ReplaceWith("expression", …)`: the expression.
fn replacement(parse: &Parse, value: SyntaxNode<'_>) -> Option<Box<str>> {
    let mut parts = named_children(value);
    let (callee, suffix) = (parts.next()?, parts.next()?);
    if parse.text_of(callee) != "ReplaceWith" {
        return None;
    }
    let arguments = named_children(suffix).find(|child| child.kind() == "value_arguments")?;
    let first = named_children(arguments).find(|child| child.kind() == "value_argument")?;
    plain_string(parse, named_children(first).last()?)
}

/// `DeprecationLevel.ERROR` or an imported `ERROR`.
fn level(written: &str) -> DeprecationLevel {
    match written.rsplit('.').next().unwrap_or(written).trim() {
        "ERROR" => DeprecationLevel::Error,
        "HIDDEN" => DeprecationLevel::Hidden,
        _ => DeprecationLevel::Warning,
    }
}
