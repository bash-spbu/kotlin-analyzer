//! `textDocument/signatureHelp`: inside a call's parentheses, the
//! signatures the callee may have and which parameter the cursor is at.

use hir::{
    FileId, ItemKind, ItemLocation, Parameter, Reference, RootDatabase, Signature, SourceDatabase,
};
use syntax::{Language, SyntaxNode, named_children};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInformation>,
    pub active_signature: usize,
    /// The parameter of the active signature the cursor's argument goes to.
    pub active_parameter: Option<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SignatureInformation {
    /// `format(money: Money, pattern: String = …): String`.
    pub label: String,
    /// Each parameter's byte range in the label.
    pub parameters: Vec<(usize, usize)>,
    parameter_names: Vec<String>,
    vararg: Option<usize>,
}

impl SignatureInformation {
    /// The parameters' names, in order.
    pub(crate) fn parameter_names(&self) -> &[String] {
        &self.parameter_names
    }
}

pub(crate) fn signature_help(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
) -> Option<SignatureHelp> {
    let source = db.source_file(file)?;
    if source.language(db) != Language::Kotlin {
        return None;
    }
    let parse = hir::parse(db, source);
    let call = match enclosing_call(parse.root(), offset) {
        Some((call, arguments)) => {
            let callee = callee_name(call)?;
            let (position, named) = argument_at(&parse, arguments, offset);
            CallAt {
                callee_start: callee.start_byte(),
                position,
                named,
            }
        }
        // `pay(1, ` while typing: the parser hasn't formed a call yet.
        None => unclosed_call(parse.text(), offset)?,
    };
    let inference = hir::infer_file(db, source);
    let locations: Vec<ItemLocation> = match &inference.name_at(call.callee_start as u32)?.reference
    {
        Reference::Declaration(location) => vec![*location],
        Reference::Candidates(candidates) => candidates.to_vec(),
        Reference::Local(_) => return None,
    };
    let signatures: Vec<SignatureInformation> = locations
        .iter()
        .flat_map(|location| signatures_of(db, *location))
        .collect();
    if signatures.is_empty() {
        return None;
    }
    let CallAt {
        position, named, ..
    } = call;
    // The first signature with room for the argument (or naming it).
    let fits = |signature: &SignatureInformation| match &named {
        Some(name) => signature
            .parameter_names
            .iter()
            .any(|parameter| parameter == name),
        None => position < signature.parameters.len() || signature.vararg.is_some(),
    };
    let active_signature = signatures.iter().position(fits).unwrap_or(0);
    let active = &signatures[active_signature];
    let active_parameter = match &named {
        Some(name) => active
            .parameter_names
            .iter()
            .position(|parameter| parameter == name),
        None => Some(match active.vararg {
            Some(vararg) if position >= vararg => vararg,
            _ => position.min(active.parameters.len().saturating_sub(1)),
        }),
    };
    Some(SignatureHelp {
        signatures,
        active_signature,
        active_parameter,
    })
}

/// Where the callee is, and which argument the cursor is in.
struct CallAt {
    callee_start: usize,
    position: usize,
    named: Option<String>,
}

/// From the text alone: the `(` before `offset` no `)` closes, the name
/// before it, and the commas between (outside nested brackets and strings).
fn unclosed_call(text: &str, offset: usize) -> Option<CallAt> {
    let before = &text[..offset.min(text.len())];
    let mut depth = 0usize;
    let mut open = None;
    for (index, char) in before.char_indices().rev() {
        match char {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' if depth > 0 => depth -= 1,
            '(' => {
                open = Some(index);
                break;
            }
            '[' | '{' => return None,
            _ => {}
        }
    }
    let open = open?;
    let name_end = before[..open].trim_end().len();
    let name_start = before[..name_end]
        .char_indices()
        .rev()
        .take_while(|(_, char)| char.is_alphanumeric() || *char == '_')
        .last()
        .map(|(index, _)| index)?;
    let (position, current) = top_level_arguments(&before[open + 1..]);
    Some(CallAt {
        callee_start: name_start,
        position,
        named: argument_name(current),
    })
}

/// The innermost call whose argument list holds `offset`, with that list.
fn enclosing_call(root: SyntaxNode<'_>, offset: usize) -> Option<(SyntaxNode<'_>, SyntaxNode<'_>)> {
    let mut current = root.descendant_for_byte_range(offset, offset);
    while let Some(node) = current {
        // Between `(` and `)` — or after `(` when the `)` isn't typed yet.
        let closed = syntax::has_token(node, ")");
        let inside = node.kind() == "value_arguments"
            && node.start_byte() < offset
            && (offset < node.end_byte() || !closed);
        if inside {
            let call = node.parent().and_then(|suffix| suffix.parent())?;
            if call.kind() == "call_expression" {
                return Some((call, node));
            }
        }
        current = node.parent();
    }
    None
}

/// The callee's name: `f` of `f(…)`, `g` of `x.g(…)`.
fn callee_name(call: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let callee = named_children(call).next()?;
    match callee.kind() {
        "simple_identifier" => Some(callee),
        "navigation_expression" => named_children(callee).last().and_then(|suffix| {
            named_children(suffix).find(|child| child.kind() == "simple_identifier")
        }),
        _ => None,
    }
}

/// Which argument the cursor is in (by position: the commas of the list
/// before it), and its name when it's written `name = …`.
fn argument_at(
    parse: &syntax::Parse,
    arguments: SyntaxNode<'_>,
    offset: usize,
) -> (usize, Option<String>) {
    let commas_before: Vec<SyntaxNode<'_>> = syntax::children(arguments)
        .filter(|child| !child.is_named() && child.kind() == "," && child.end_byte() <= offset)
        .collect();
    let position = commas_before.len();
    // The argument begun after the last comma before the cursor: its text
    // up to the cursor says whether it's named (`currency = ` parses as an
    // error while its value isn't typed, so the text, not the node).
    let begun_at = commas_before
        .last()
        .map_or(arguments.start_byte() + 1, |comma| comma.end_byte());
    let text = parse.text();
    let named = argument_name(&text[begun_at.min(offset)..offset.min(text.len())]);
    (position, named)
}

/// How many top-level commas `text` (a call's arguments up to the cursor)
/// has, and the text of the argument the cursor is in.
fn top_level_arguments(text: &str) -> (usize, &str) {
    let mut depth = 0usize;
    let mut commas = 0;
    let mut current_start = 0;
    let mut in_string = false;
    let mut in_char = false;
    for (index, char) in text.char_indices() {
        match char {
            '"' if !in_char => in_string = !in_string,
            '\'' if !in_string => in_char = !in_char,
            _ if in_string || in_char => {}
            // `<` and `>` aren't counted: `a < b, c` is two arguments.
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                commas += 1;
                current_start = index + 1;
            }
            _ => {}
        }
    }
    (commas, &text[current_start..])
}

/// `name` of an argument written `name = …`.
fn argument_name(argument: &str) -> Option<String> {
    let (name, _) = argument.split_once('=')?;
    let name = name.trim();
    let is_name = !name.is_empty()
        && name
            .chars()
            .all(|char| char.is_alphanumeric() || char == '_');
    let is_comparison = argument.contains("==");
    (is_name && !is_comparison).then(|| name.to_owned())
}

/// The signatures calling `location` can mean: a function's, or a class's
/// constructors.
fn signatures_of(db: &RootDatabase, location: ItemLocation) -> Vec<SignatureInformation> {
    let tree = hir::item_tree(db, location.container);
    let item = tree.item(location.item);
    let name = item.name.to_string();
    match (&item.kind, &item.signature) {
        (
            ItemKind::Function | ItemKind::Constructor,
            Signature::Function {
                parameters,
                return_type,
                ..
            },
        ) => {
            let result = return_type
                .as_ref()
                .map(hir::type_ref_text)
                .map(|text| shorten(&text));
            vec![signature(&name, parameters, result.as_deref())]
        }
        (
            ItemKind::Class(_),
            Signature::Class {
                primary_constructor,
                ..
            },
        ) => {
            let mut signatures: Vec<SignatureInformation> = primary_constructor
                .iter()
                .map(|parameters| signature(&name, parameters, None))
                .collect();
            for child in item.children.iter() {
                let constructor = tree.item(*child);
                if let (ItemKind::Constructor, Signature::Function { parameters, .. }) =
                    (&constructor.kind, &constructor.signature)
                {
                    signatures.push(signature(&name, parameters, None));
                }
            }
            if signatures.is_empty() {
                signatures.push(signature(&name, &[], None));
            }
            signatures
        }
        _ => Vec::new(),
    }
}

fn signature(name: &str, parameters: &[Parameter], result: Option<&str>) -> SignatureInformation {
    let mut label = format!("{name}(");
    let mut ranges = Vec::with_capacity(parameters.len());
    let mut vararg = None;
    for (index, parameter) in parameters.iter().enumerate() {
        if index > 0 {
            label.push_str(", ");
        }
        let start = label.len();
        if parameter.vararg {
            label.push_str("vararg ");
            vararg = Some(index);
        }
        let written = parameter
            .type_ref
            .as_ref()
            .map(hir::type_ref_text)
            .map(|text| shorten(&text))
            .unwrap_or_else(|| "?".to_owned());
        label.push_str(&format!("{}: {written}", parameter.name));
        if parameter.has_default {
            label.push_str(" = …");
        }
        ranges.push((start, label.len()));
    }
    label.push(')');
    if let Some(result) = result {
        label.push_str(&format!(": {result}"));
    }
    SignatureInformation {
        label,
        parameters: ranges,
        parameter_names: parameters
            .iter()
            .map(|parameter| parameter.name.to_string())
            .collect(),
        vararg,
    }
}

/// `java.time.LocalDate` → `LocalDate`: package qualifiers dropped.
pub(crate) fn shorten(text: &str) -> String {
    let mut shortened = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, shortened: &mut String| {
        let segments: Vec<&str> = word.split('.').collect();
        let first_class = segments
            .iter()
            .position(|segment| segment.chars().next().is_some_and(char::is_uppercase))
            .unwrap_or(0);
        shortened.push_str(&segments[first_class..].join("."));
        word.clear();
    };
    for char in text.chars() {
        if char.is_alphanumeric() || char == '_' || char == '.' {
            word.push(char);
        } else {
            flush(&mut word, &mut shortened);
            shortened.push(char);
        }
    }
    flush(&mut word, &mut shortened);
    shortened
}

#[cfg(test)]
mod tests {
    use super::shorten;

    #[test]
    fn package_qualifiers_go_and_nesting_stays() {
        assert_eq!(
            shorten("kotlin.collections.Map.Entry<kotlin.String, java.time.LocalDate?>"),
            "Map.Entry<String, LocalDate?>"
        );
        assert_eq!(shorten("T"), "T");
    }
}
