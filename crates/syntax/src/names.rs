//! Names as Kotlin source spells them: a hard keyword, or anything that
//! isn't a plain identifier, is written in backticks (`` `in` ``,
//! `` `my test` ``).

use std::borrow::Cow;

/// Words Kotlin never takes as identifiers without backticks.
pub const HARD_KEYWORDS: &[&str] = &[
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

/// A letter or `_`, then letters, digits and `_` — and not a keyword.
pub fn is_plain_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let starts_well = chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_');
    starts_well
        && chars.all(|char| char.is_alphanumeric() || char == '_')
        && !HARD_KEYWORDS.contains(&name)
}

/// `name` as source must write it.
pub fn escaped(name: &str) -> Cow<'_, str> {
    if is_plain_identifier(name) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(format!("`{name}`"))
    }
}

/// A dotted name (`a.in.Money`) with each segment as source must write it
/// (`` a.`in`.Money ``).
pub fn escaped_path(dotted: &str) -> String {
    dotted.split('.').map(escaped).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_odd_names_get_backticks() {
        assert_eq!(escaped("money"), "money");
        assert_eq!(escaped("in"), "`in`");
        assert_eq!(escaped("my test"), "`my test`");
        assert_eq!(
            escaped_path("com.example.in.Money"),
            "com.example.`in`.Money"
        );
        assert!(!is_plain_identifier("1st"));
    }
}
