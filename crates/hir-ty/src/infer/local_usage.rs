//! Locals a body declares and never reads (Kotlin's `UNUSED_VARIABLE`), and
//! `var`s it never assigns again (`CAN_BE_VAL`).
//!
//! A local counts as read when inference recorded a use of it, or when its
//! name appears anywhere else in the enclosing declaration — in a string
//! template, in code inference couldn't follow, or as a namesake. Only a
//! local nothing could read is reported.

use base_db::{SourceDatabase, SourceFile};
use rustc_hash::FxHashSet;
use syntax::{Language, SyntaxNode, TextRange, has_token, named_children};

use super::declarations::is_script;
use super::member_scope::data_flow_scope;
use super::{LocalId, LocalKind, Reference, infer_file};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LocalFinding {
    /// `val name = …` whose name is read nowhere.
    Unused { name: TextRange },
    /// `var name = …` never assigned again: its `var` keyword.
    NeverReassigned { keyword: TextRange },
}

/// The unused locals and never-reassigned `var`s of `file`.
pub fn local_findings(db: &dyn SourceDatabase, file: SourceFile) -> Vec<LocalFinding> {
    if file.language(db) != Language::Kotlin {
        return Vec::new();
    }
    let parse = base_db::parse(db, file);
    // A script's top-level `val`s are its properties, not locals.
    if is_script(&parse) {
        return Vec::new();
    }
    let inference = infer_file(db, file);
    let used: FxHashSet<LocalId> = inference
        .names()
        .filter_map(|(_, name)| match name.reference {
            Reference::Local(local) => Some(local),
            _ => None,
        })
        .collect();
    let mut findings = Vec::new();
    for (id, local) in inference.locals() {
        if !matches!(local.kind, LocalKind::Value | LocalKind::Variable) {
            continue;
        }
        let Some(declaration) = local_property(parse.root(), local.name_range) else {
            continue;
        };
        let name = local.name.as_str();
        if name == "_" {
            continue;
        }
        let scope = data_flow_scope(declaration);
        let mentioned = mentioned_elsewhere(parse.text(), scope, name, local.name_range);
        if !used.contains(&id) && !mentioned {
            findings.push(LocalFinding::Unused {
                name: local.name_range,
            });
            continue;
        }
        let is_var = local.kind == LocalKind::Variable;
        if is_var && has_token(declaration, "=") && !assigned_in(&parse, scope, name) {
            let keyword = named_children(declaration)
                .find(|child| child.kind() == "binding_pattern_kind")
                .and_then(|binding| syntax::token(binding, "var"));
            if let Some(keyword) = keyword {
                findings.push(LocalFinding::NeverReassigned {
                    keyword: TextRange::of(keyword),
                });
            }
        }
    }
    findings
}

/// The `val`/`var` statement in a body that declares the name at
/// `name_range` — one name, not destructured, not delegated (`by lazy`).
fn local_property(root: SyntaxNode<'_>, name_range: TextRange) -> Option<SyntaxNode<'_>> {
    let name =
        root.named_descendant_for_byte_range(name_range.start as usize, name_range.end as usize)?;
    let variable = name
        .parent()
        .filter(|node| node.kind() == "variable_declaration")?;
    let property = variable
        .parent()
        .filter(|node| node.kind() == "property_declaration")?;
    let in_body = property
        .parent()
        .is_some_and(|parent| parent.kind() == "statements");
    let delegated = named_children(property).any(|child| child.kind() == "property_delegate");
    (in_body && !delegated).then_some(property)
}

/// Whether `name` appears as a whole word in `scope` other than at the
/// declaration.
fn mentioned_elsewhere(text: &str, scope: SyntaxNode<'_>, name: &str, declared: TextRange) -> bool {
    let scope_text = &text[scope.byte_range()];
    let is_word = |char: char| char.is_alphanumeric() || char == '_';
    scope_text.match_indices(name).any(|(offset, _)| {
        let start = scope.start_byte() + offset;
        let before = scope_text[..offset].chars().next_back();
        let after = scope_text[offset + name.len()..].chars().next();
        let whole_word = !before.is_some_and(is_word) && !after.is_some_and(is_word);
        whole_word && start != declared.start as usize
    })
}

/// Whether `scope` assigns a variable named `name` anywhere: `name = …`,
/// `name += …`, `name++`, `--name` — any namesake counts.
fn assigned_in(parse: &syntax::Parse, scope: SyntaxNode<'_>, name: &str) -> bool {
    let names = |node: SyntaxNode<'_>| {
        named_children(node).next().is_some_and(|first| {
            first.kind() == "simple_identifier" && parse.text_of(first).trim_matches('`') == name
        })
    };
    syntax::descendants(scope).any(|node| match node.kind() {
        "assignment" => named_children(node).next().is_some_and(|target| {
            target.kind() == "directly_assignable_expression" && names(target)
        }),
        "postfix_expression" | "prefix_expression" => {
            let steps = has_token(node, "++") || has_token(node, "--");
            steps && names(node)
        }
        _ => false,
    })
}
