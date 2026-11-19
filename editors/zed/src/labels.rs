//! Completion and symbol labels highlighted as Kotlin: each is a line of
//! Kotlin code (`fun total(values: List<Money>): Long {}`) that Zed parses
//! and colours, of which the label shows a part (`total(values:
//! List<Money>): Long`), and — less prominently — where it's declared.
//! Plain Rust, so it's tested on the host as well.

use std::ops::Range;

/// What a completion or a symbol names, as far as its label goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Declared {
    Function,
    /// A property, a field, a local, a parameter.
    Value,
    /// A class, an interface, an enum, a type alias.
    Type,
    EnumEntry,
}

/// A label: `code` to parse, the part of it shown, and what follows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Label {
    pub code: String,
    /// What's shown of `code`; it starts with the name.
    pub shown: Range<usize>,
    /// Where it's declared (`app.payments`, `import app.Money`), shown
    /// after it as a comment.
    pub description: Option<String>,
    /// The name, which is what typing filters by, in what's shown.
    pub filter: Range<usize>,
}

/// A completion's label from the server's `label`, `labelDetails.detail`
/// (the rest of the signature: `(amount: Long): Unit`, `: String`) and
/// `labelDetails.description`.
pub fn completion_label(
    declared: Declared,
    name: &str,
    signature_rest: Option<&str>,
    description: Option<&str>,
) -> Option<Label> {
    let is_plain_name = !name.is_empty() && !name.contains(['=', ' ', '\n']);
    if !is_plain_name && !is_quoted(name) {
        return None;
    }
    let rest = signature_rest.unwrap_or_default();
    let (before, after) = match declared {
        Declared::Function => {
            let parameters = if rest.is_empty() { "()" } else { "" };
            ("fun ", format!("{rest}{parameters} {{}}"))
        }
        Declared::Value => ("val ", rest.to_owned()),
        Declared::Type => ("class ", rest.to_owned()),
        Declared::EnumEntry => ("enum class Entries { ", " }".to_owned()),
    };
    let code = format!("{before}{name}{after}");
    let shown_end = match declared {
        // The body that makes it parse isn't shown.
        Declared::Function => code.len() - " {}".len(),
        Declared::EnumEntry => before.len() + name.len(),
        Declared::Value | Declared::Type => code.len(),
    };
    Some(Label {
        shown: before.len()..shown_end,
        code,
        description: description
            .filter(|description| !description.is_empty())
            .map(str::to_owned),
        filter: 0..name.len(),
    })
}

/// A workspace symbol's label: its name, coloured as what it is.
pub fn symbol_label(declared: Declared, name: &str) -> Option<Label> {
    let mut label = completion_label(declared, name, None, None)?;
    label.shown = label.shown.start..label.shown.start + name.len();
    Some(label)
}

/// A name in backticks (`` `creates an order` ``).
fn is_quoted(name: &str) -> bool {
    name.len() > 2 && name.starts_with('`') && name.ends_with('`') && !name.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(label: &Label) -> &str {
        &label.code[label.shown.clone()]
    }

    #[test]
    fn a_function_shows_its_signature_from_its_name_and_where_it_is_declared() {
        let label = completion_label(
            Declared::Function,
            "total",
            Some("(values: List<Money>): Long"),
            Some("app.payments.Ledger"),
        )
        .unwrap();
        assert_eq!(label.code, "fun total(values: List<Money>): Long {}");
        assert_eq!(shown(&label), "total(values: List<Money>): Long");
        assert_eq!(label.description.as_deref(), Some("app.payments.Ledger"));
        assert_eq!(&shown(&label)[label.filter.clone()], "total");
    }

    #[test]
    fn values_types_and_enum_entries_parse_as_their_declarations() {
        let value = completion_label(Declared::Value, "count", Some(": Int"), None).unwrap();
        assert_eq!(value.code, "val count: Int");
        assert_eq!(shown(&value), "count: Int");
        assert_eq!(value.description, None);

        let class =
            completion_label(Declared::Type, "Money", None, Some("import app.Money")).unwrap();
        assert_eq!(class.code, "class Money");
        assert_eq!(shown(&class), "Money");

        let entry = completion_label(Declared::EnumEntry, "ACTIVE", None, Some("Status")).unwrap();
        assert_eq!(entry.code, "enum class Entries { ACTIVE }");
        assert_eq!(shown(&entry), "ACTIVE");

        let without_parameters = completion_label(Declared::Function, "run", None, None).unwrap();
        assert_eq!(shown(&without_parameters), "run()");
    }

    #[test]
    fn a_named_argument_or_odd_label_keeps_zeds_own_and_quoted_names_are_names() {
        assert_eq!(
            completion_label(Declared::Value, "count = ", None, None),
            None
        );
        assert_eq!(completion_label(Declared::Value, "", None, None), None);
        let quoted =
            completion_label(Declared::Function, "`creates an order`", Some("()"), None).unwrap();
        assert_eq!(shown(&quoted), "`creates an order`()");
    }

    #[test]
    fn a_symbol_shows_only_its_name() {
        let label = symbol_label(Declared::Function, "total").unwrap();
        assert_eq!(label.code, "fun total() {}");
        assert_eq!(shown(&label), "total");
    }
}
