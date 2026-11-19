//! What a `when` with a subject says twice or needlessly: a condition an
//! earlier one already checks (the same constant, enum entry, object or
//! `is` type), an `else` after branches covering every case.

use hir_def::ItemLocation;
use hir_def::item_tree::{ClassKind, ItemKind};
use syntax::ast::kotlin::TypeTest;
use syntax::{SyntaxNode, TextRange, named_children};

use super::body_facts::BodyFacts;
use super::{Reference, WhenFinding};
use crate::ty::Ty;

/// What a branch condition compares the subject with, as the compiler
/// tells two apart.
#[derive(PartialEq, Eq)]
enum Condition {
    /// A literal, by its kind and text: the same text is the same value.
    Literal { kind: &'static str, text: String },
    /// An enum entry, or an object.
    Declaration(ItemLocation),
    /// `is T` / `!is T`, by the type's text without spaces — or by the type
    /// itself when it's known whole (`List<String>` and an alias of it).
    TypeTest {
        negated: bool,
        text: String,
        ty: Option<Ty>,
    },
}

impl Condition {
    fn duplicates(&self, earlier: &Condition) -> bool {
        match (self, earlier) {
            (
                Condition::TypeTest { negated, text, ty },
                Condition::TypeTest {
                    negated: earlier_negated,
                    text: earlier_text,
                    ty: earlier_ty,
                },
            ) => {
                let same_type = text == earlier_text || ty.is_some() && ty == earlier_ty;
                negated == earlier_negated && same_type
            }
            _ => self == earlier,
        }
    }
}

const LITERALS: [&str; 9] = [
    "integer_literal",
    "long_literal",
    "hex_literal",
    "bin_literal",
    "unsigned_literal",
    "real_literal",
    "boolean_literal",
    "character_literal",
    "null_literal",
];

impl BodyFacts<'_, '_> {
    /// Conditions an earlier one in the same `when` already checks. A
    /// branch with a guard (`is T if …`) isn't a plain check of its own.
    pub(super) fn duplicate_conditions(&self, node: SyntaxNode<'_>) -> Vec<TextRange> {
        let mut seen: Vec<Condition> = Vec::new();
        let mut duplicates = Vec::new();
        let entries = named_children(node).filter(|child| child.kind() == "when_entry");
        for entry in entries.filter(|entry| !syntax::ast::kotlin::has_guard(*entry)) {
            for condition in named_children(entry).filter(|child| child.kind() == "when_condition")
            {
                let Some(value) = named_children(condition).next() else {
                    continue;
                };
                let Some((compared, range)) = self.compared(value) else {
                    continue;
                };
                if seen.iter().any(|earlier| compared.duplicates(earlier)) {
                    duplicates.push(range);
                } else {
                    seen.push(compared);
                }
            }
        }
        duplicates
    }

    /// What a condition compares the subject with, and where the compiler
    /// reports it twice: the value, or the `is` type.
    fn compared(&self, value: SyntaxNode<'_>) -> Option<(Condition, TextRange)> {
        let kind = value.kind();
        if let Some(literal) = LITERALS.iter().find(|literal| **literal == kind) {
            let literal = Condition::Literal {
                kind: literal,
                text: self.text(value).to_owned(),
            };
            return Some((literal, TextRange::of(value)));
        }
        match kind {
            "string_literal" => {
                let interpolated = named_children(value).any(|part| {
                    matches!(
                        part.kind(),
                        "interpolated_identifier" | "interpolated_expression"
                    )
                });
                let literal = Condition::Literal {
                    kind: "string_literal",
                    text: self.text(value).to_owned(),
                };
                (!interpolated).then(|| (literal, TextRange::of(value)))
            }
            "simple_identifier" | "navigation_expression" => {
                let name = match kind {
                    "simple_identifier" => value,
                    _ => named_children(named_children(value).last()?)
                        .find(|child| child.kind() == "simple_identifier")?,
                };
                let Reference::Declaration(location) = self.recorded_at(name)?.reference else {
                    return None;
                };
                let tree = hir_def::item_tree(self.db, location.container);
                let is_constant = matches!(
                    tree.item(location.item).kind,
                    ItemKind::EnumEntry | ItemKind::Class(ClassKind::Object)
                );
                is_constant.then(|| (Condition::Declaration(location), TextRange::of(value)))
            }
            "type_test" => {
                let target = self.type_child(value)?;
                let ty = self.written_type(target);
                let text: String = self
                    .text(target)
                    .chars()
                    .filter(|char| !char.is_whitespace())
                    .collect();
                let test = Condition::TypeTest {
                    negated: TypeTest::of(value) == TypeTest::IsNot,
                    text,
                    ty: (!ty.has_unknown_part()).then_some(ty),
                };
                Some((test, TextRange::of(target)))
            }
            _ => None,
        }
    }
}

/// The `else` branch of `node`: its keyword, and the whole entry to remove.
pub(super) fn redundant_else(node: SyntaxNode<'_>) -> Option<WhenFinding> {
    let entry = named_children(node).find(|child| {
        child.kind() == "when_entry"
            && !named_children(*child).any(|part| part.kind() == "when_condition")
    })?;
    let keyword = syntax::token(entry, "else")?;
    Some(WhenFinding::RedundantElse {
        keyword: TextRange::of(keyword),
        entry: TextRange::of(entry),
    })
}
