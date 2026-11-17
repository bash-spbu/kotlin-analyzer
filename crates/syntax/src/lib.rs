//! Syntax trees for Kotlin and Java on top of tree-sitter, and a typed view
//! of them ([`ast`]): `ast::kotlin::ClassDeclaration::name()` instead of
//! matching node kind strings. Pure functions of source text — no index, no
//! other files.

pub mod ast;
pub mod names;
mod pointer;
mod range;

use std::path::Path;
use std::sync::Arc;

pub use pointer::SyntaxNodePtr;
pub use range::TextRange;
pub use tree_sitter::Node as SyntaxNode;

/// The languages a source file can be in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Language {
    Kotlin,
    Java,
}

impl Language {
    /// The language of a file by its extension; `None` for anything else.
    pub fn of_path(path: &Path) -> Option<Language> {
        match path.extension()?.to_str()? {
            "kt" | "kts" => Some(Language::Kotlin),
            "java" => Some(Language::Java),
            _ => None,
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Language::Kotlin => tree_sitter_kotlin::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }
}

/// A parsed file: its text and syntax tree.
#[derive(Clone)]
pub struct Parse {
    text: Arc<str>,
    tree: tree_sitter::Tree,
    language: Language,
}

impl std::fmt::Debug for Parse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Parse")
            .field("language", &self.language)
            .field("length", &self.text.len())
            .finish()
    }
}

/// Two parses of the same text are the same parse.
impl PartialEq for Parse {
    fn eq(&self, other: &Parse) -> bool {
        self.language == other.language && self.text == other.text
    }
}

impl Eq for Parse {}

/// Parses `text`. Tree-sitter always produces a tree; what it couldn't
/// parse shows up as error nodes (see [`Parse::errors`]).
pub fn parse(text: Arc<str>, language: Language) -> Parse {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language.grammar())
        .expect("the bundled grammar matches the tree-sitter version");
    let parsed_text = match language {
        Language::Kotlin => soft_keywords_as_names(&text),
        Language::Java => std::borrow::Cow::Borrowed(&*text),
    };
    let tree = parser
        .parse(parsed_text.as_bytes(), None)
        .expect("parsing without a timeout or cancellation flag always yields a tree");
    Parse {
        text,
        tree,
        language,
    }
}

/// `text` as the grammar is given it: `where` — a soft keyword, which
/// Kotlin code may name a value (`val where = WhereBuilder()`), but the
/// grammar always reads as the keyword — spelled differently where it can
/// only be a name: followed by `.`, `?`, `!`, `)`, `,`, `[`, `]`, `=`, `:`
/// or `;` (a type constraint, `where T : Bound`, is followed by a name).
/// The replacement has the same length, so every node keeps its range, and
/// the [`Parse`] keeps the original text.
fn soft_keywords_as_names(text: &str) -> std::borrow::Cow<'_, str> {
    const KEYWORD: &str = "where";
    const AS_NAME: &str = "wherE";
    let is_name_char = |char: char| char.is_alphanumeric() || char == '_' || char == '`';
    let mut replaced: Option<String> = None;
    for (start, _) in text.match_indices(KEYWORD) {
        let end = start + KEYWORD.len();
        let starts_word = !text[..start].chars().next_back().is_some_and(is_name_char);
        let ends_word = !text[end..].chars().next().is_some_and(is_name_char);
        let next = text[end..].chars().find(|char| !char.is_whitespace());
        let used_as_name = next.is_some_and(|next| ".?!),[]=:;".contains(next));
        if starts_word && ends_word && used_as_name {
            replaced
                .get_or_insert_with(|| text.to_owned())
                .replace_range(start..end, AS_NAME);
        }
    }
    match replaced {
        Some(text) => std::borrow::Cow::Owned(text),
        None => std::borrow::Cow::Borrowed(text),
    }
}

impl Parse {
    pub fn root(&self) -> SyntaxNode<'_> {
        self.tree.root_node()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn shared_text(&self) -> &Arc<str> {
        &self.text
    }

    pub fn language(&self) -> Language {
        self.language
    }

    /// The source text of `node`.
    pub fn text_of(&self, node: SyntaxNode<'_>) -> &str {
        &self.text[node.byte_range()]
    }

    /// Every place the parser couldn't make sense of.
    pub fn errors(&self) -> Vec<SyntaxError> {
        let mut errors = Vec::new();
        for node in descendants(self.root()) {
            if node.is_missing() {
                errors.push(SyntaxError {
                    range: TextRange::of(node),
                    message: format!("missing `{}`", node.kind()),
                });
            } else if node.is_error() && GrammarGap::of(node, &self.text).is_none() {
                errors.push(SyntaxError {
                    range: TextRange::of(node),
                    message: "syntax error".to_owned(),
                });
            }
        }
        errors
    }

    /// The tree as indented text, for debugging and snapshot tests.
    pub fn debug_tree(&self) -> String {
        let mut output = String::new();
        let mut depth = 0usize;
        let mut cursor = self.root().walk();
        loop {
            let node = cursor.node();
            if node.is_named() || node.is_missing() {
                let label = match node.kind() {
                    _ if node.is_missing() => format!("MISSING {}", node.kind()),
                    "simple_identifier" | "type_identifier" | "identifier" => {
                        format!("{} {:?}", node.kind(), self.text_of(node))
                    }
                    kind => kind.to_owned(),
                };
                output.push_str(&"  ".repeat(depth));
                output.push_str(&label);
                output.push('\n');
            }
            if cursor.goto_first_child() {
                depth += 1;
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return output;
                }
                depth -= 1;
            }
        }
    }
}

/// A place the parser rejected.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SyntaxError {
    pub range: TextRange,
    pub message: String,
}

/// Kotlin the grammar doesn't know, where it leaves an error node though
/// the code is valid.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrammarGap {
    /// An anonymous function's parameters without types (`fun(_, value) =
    /// …`, which Kotlin allows where the function's type is known).
    UntypedAnonymousFunctionParameters,
    /// A guard on a `when`'s `else` branch (`else if ready -> …`, Kotlin
    /// 2.2); the grammar knows guards after other conditions only.
    GuardedElse,
    /// A lambda whose first statement starts with a parenthesized
    /// expression (`{ (handler)(call, it) }`), which the grammar takes for
    /// destructured parameters though no `->` follows.
    LambdaStartingWithParentheses,
}

impl GrammarGap {
    /// The gap `error` is, if it is one.
    pub fn of(error: SyntaxNode<'_>, text: &str) -> Option<GrammarGap> {
        let parent = error.parent()?;
        let in_anonymous_function_parameters = parent.kind() == "function_value_parameters"
            && parent
                .parent()
                .is_some_and(|function| function.kind() == "anonymous_function");
        if in_anonymous_function_parameters
            && named_children(error).all(|child| child.kind() == "simple_identifier")
        {
            return Some(GrammarGap::UntypedAnonymousFunctionParameters);
        }
        let written: String = text[error.byte_range()]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if parent.kind() == "when_expression" && written == "else if" {
            return Some(GrammarGap::GuardedElse);
        }
        // `(handler)(call, it)` read as destructuring: one name in the first
        // parentheses — the callee — then the call's arguments. Two names
        // first (`{ (a, b) }`) is a real error.
        let mut groups = named_children(error);
        let callee_alone = groups.next().is_some_and(|first| {
            first.kind() == "multi_variable_declaration" && named_children(first).count() == 1
        });
        let destructures_only =
            callee_alone && groups.all(|child| child.kind() == "multi_variable_declaration");
        if parent.kind() == "lambda_literal" && destructures_only && !has_token(parent, "->") {
            return Some(GrammarGap::LambdaStartingWithParentheses);
        }
        None
    }
}

/// Every node under `root` (itself included), in document order, walked
/// with a cursor so deep trees don't grow the stack.
pub fn descendants(root: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    descendants_pruned(root, |_| false)
}

/// Like [`descendants`], but a node `prune` takes is yielded without its
/// subtree.
pub fn descendants_pruned<'tree>(
    root: SyntaxNode<'tree>,
    prune: impl Fn(SyntaxNode<'tree>) -> bool,
) -> impl Iterator<Item = SyntaxNode<'tree>> {
    let mut cursor = root.walk();
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let node = cursor.node();
        if !prune(node) && cursor.goto_first_child() {
            return Some(node);
        }
        loop {
            if cursor.goto_next_sibling() {
                return Some(node);
            }
            if !cursor.goto_parent() {
                finished = true;
                return Some(node);
            }
        }
    })
}

/// The named children of `node` of kind `kind`.
pub fn children_of_kind<'tree>(
    node: SyntaxNode<'tree>,
    kind: &'static str,
) -> impl Iterator<Item = SyntaxNode<'tree>> {
    named_children(node).filter(move |child| child.kind() == kind)
}

/// The next named sibling of `node`, past comments (see
/// [`named_children`]).
pub fn next_named_sibling(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut next = node.next_named_sibling();
    while let Some(sibling) = next.filter(|sibling| sibling.is_extra()) {
        next = sibling.next_named_sibling();
    }
    next
}

/// The previous named sibling of `node`, past comments (see
/// [`named_children`]).
pub fn previous_named_sibling(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let mut previous = node.prev_named_sibling();
    while let Some(sibling) = previous.filter(|sibling| sibling.is_extra()) {
        previous = sibling.prev_named_sibling();
    }
    previous
}

/// The named children of `node`, without comments. A comment is an extra
/// in both grammars: it may sit between any two nodes, so the first child
/// of a body, a lambda or parentheses can be a comment instead of the code.
pub fn named_children(node: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    children(node).filter(|child| child.is_named() && !child.is_extra())
}

/// Every child of `node`, named or not, walked with a cursor: asking for a
/// child by index costs as much as walking to it.
pub fn children(node: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    let mut cursor = node.walk();
    let mut position = ChildPosition::BeforeFirst;
    std::iter::from_fn(move || {
        let moved = match position {
            ChildPosition::BeforeFirst => cursor.goto_first_child(),
            ChildPosition::At => cursor.goto_next_sibling(),
            ChildPosition::Past => false,
        };
        if !moved {
            position = ChildPosition::Past;
            return None;
        }
        position = ChildPosition::At;
        Some(cursor.node())
    })
}

/// Where a walk over a node's children is.
#[derive(Clone, Copy)]
enum ChildPosition {
    BeforeFirst,
    At,
    Past,
}

/// Whether `node` has an anonymous child token spelled `token`
/// (`class`, `interface`, `enum`, …).
pub fn has_token(node: SyntaxNode<'_>, token: &str) -> bool {
    children(node).any(|child| !child.is_named() && child.kind() == token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn siblings_and_children_skip_comments() {
        let text = "class A {\n    val a: Int\n        // read\n        get() = 1\n}\n";
        let parse = parse(Arc::from(text), Language::Kotlin);
        let nodes: Vec<SyntaxNode<'_>> = descendants(parse.root()).collect();
        let find = |kind: &str| *nodes.iter().find(|node| node.kind() == kind).unwrap();
        let (property, getter) = (find("property_declaration"), find("getter"));
        assert_eq!(next_named_sibling(property), Some(getter));
        assert_eq!(previous_named_sibling(getter), Some(property));
        assert_eq!(
            property.next_named_sibling().unwrap().kind(),
            "line_comment"
        );
        let body = find("class_body");
        let kinds: Vec<&str> = named_children(body).map(|child| child.kind()).collect();
        assert_eq!(kinds, ["property_declaration", "getter"]);
    }

    #[test]
    fn errors_report_broken_code_and_nothing_for_valid_code() {
        let valid = parse(Arc::from("class A { fun f() = 1 }\n"), Language::Kotlin);
        assert!(valid.errors().is_empty(), "{:?}", valid.errors());
        let broken = parse(Arc::from("class A { fun f( }\n"), Language::Kotlin);
        assert!(!broken.errors().is_empty());
    }

    #[test]
    fn where_named_like_a_value_parses_and_constraints_stay_constraints() {
        let code = "fun f() {\n    val where = Builder()\n    where.and(1)\n    where\n        .or(2)\n    use(where, where?.x, where!!)\n}\nfun <T> g(t: T): T where T : Comparable<T> = t\n";
        let parse = parse(Arc::from(code), Language::Kotlin);
        assert!(parse.errors().is_empty(), "{:?}", parse.errors());
        assert_eq!(parse.text(), code);
        let names: Vec<&str> = descendants(parse.root())
            .filter(|node| node.kind() == "simple_identifier")
            .map(|node| parse.text_of(node))
            .filter(|name| *name == "where")
            .collect();
        assert_eq!(names.len(), 6);
        assert!(descendants(parse.root()).any(|node| node.kind() == "type_constraints"));
    }

    #[test]
    fn an_anonymous_functions_untyped_parameters_are_valid_kotlin() {
        let valid = parse(
            Arc::from("fun f() { listen(fun(_, value) = load(value)) }\n"),
            Language::Kotlin,
        );
        assert!(valid.errors().is_empty(), "{:?}", valid.errors());
        let broken = parse(
            Arc::from("fun f() { listen(fun(1 +) = 2) }\n"),
            Language::Kotlin,
        );
        assert!(!broken.errors().is_empty());
    }

    #[test]
    fn a_guarded_else_and_a_lambda_starting_with_parentheses_are_valid_kotlin() {
        let errors = |text: &str| parse(Arc::from(text), Language::Kotlin).errors();
        let valid = "fun f(x: Any, y: Int?) = when (x) {\n    is String if x.isEmpty() -> 1\n    else if y != null -> 2\n    else -> 3\n}\n\
                     fun g(c: String?, h: (String) -> Int) = c?.let { (h)(it) }\n\
                     fun k(c: String?, h: (String, String) -> Int) = c?.let { (h)(c, it) }\n";
        assert!(errors(valid).is_empty(), "{:?}", errors(valid));
        // An `if`-`else` chain is still parsed as one.
        let chain = "fun h(a: Boolean) = if (a) 1\nelse if (!a) 2\nelse 3\n";
        assert!(errors(chain).is_empty(), "{:?}", errors(chain));
        // Two names in parentheses with no `->` are no expression.
        assert!(!errors("fun g() = run { (a, b) }\n").is_empty());
        assert!(!errors("fun f(x: Any) = when (x) { else if -> 1 }\n").is_empty());
    }

    #[test]
    fn descendants_visit_every_node_once_in_document_order() {
        let parse = parse(Arc::from("package a.b\nclass C\n"), Language::Kotlin);
        let starts: Vec<usize> = descendants(parse.root())
            .map(|node| node.start_byte())
            .collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted);
        assert_eq!(
            descendants(parse.root()).count(),
            descendants(parse.root())
                .map(|node| node.id())
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(Language::of_path(Path::new("A.kt")), Some(Language::Kotlin));
        assert_eq!(
            Language::of_path(Path::new("build.gradle.kts")),
            Some(Language::Kotlin)
        );
        assert_eq!(Language::of_path(Path::new("A.java")), Some(Language::Java));
        assert_eq!(Language::of_path(Path::new("A.swift")), None);
    }
}
