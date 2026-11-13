//! Syntax trees for Kotlin and Java on top of tree-sitter, and a typed view
//! of them ([`ast`]): `ast::kotlin::ClassDeclaration::name()` instead of
//! matching node kind strings. Pure functions of source text — no index, no
//! other files.

pub mod ast;
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
    let tree = parser
        .parse(text.as_bytes(), None)
        .expect("parsing without a timeout or cancellation flag always yields a tree");
    Parse {
        text,
        tree,
        language,
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
            } else if node.is_error() {
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

/// Every node under `root` (itself included), in document order, walked
/// with a cursor so deep trees don't grow the stack.
pub fn descendants(root: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    let mut cursor = root.walk();
    let mut finished = false;
    std::iter::from_fn(move || {
        if finished {
            return None;
        }
        let node = cursor.node();
        if cursor.goto_first_child() {
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

/// The named children of `node`.
pub fn named_children(node: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    let count = node.named_child_count();
    (0..count).filter_map(move |index| node.named_child(index as u32))
}

/// Whether `node` has an anonymous child token spelled `token`
/// (`class`, `interface`, `enum`, …).
pub fn has_token(node: SyntaxNode<'_>, token: &str) -> bool {
    let count = node.child_count();
    (0..count)
        .filter_map(|index| node.child(index as u32))
        .any(|child| !child.is_named() && child.kind() == token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_report_broken_code_and_nothing_for_valid_code() {
        let valid = parse(Arc::from("class A { fun f() = 1 }\n"), Language::Kotlin);
        assert!(valid.errors().is_empty(), "{:?}", valid.errors());
        let broken = parse(Arc::from("class A { fun f( }\n"), Language::Kotlin);
        assert!(!broken.errors().is_empty());
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
