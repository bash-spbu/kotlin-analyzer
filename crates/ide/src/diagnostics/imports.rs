//! Names nothing resolves (`missing-import` when an import would fix it,
//! `unresolved-reference` otherwise) and imports nothing uses
//! (`unused-import`).

use hir::{
    Certainty, ItemKind, ItemLocation, Name, NameSite, Path, Resolution, RootDatabase, Signature,
    SourceFile, Ty,
};
use rustc_hash::FxHashSet;
use syntax::{Parse, SyntaxNode, TextRange, has_token, named_children};

use super::{Check, Diagnostic, DiagnosticsConfig, Fix, TextEdit};

/// Functions called through operators and conventions (`a + b`, `by
/// lazy`, `for (x in xs)`, `val (a, b) = …`): an import of one is used
/// without its name appearing.
const CONVENTION_NAMES: &[&str] = &[
    "plus",
    "minus",
    "times",
    "div",
    "rem",
    "mod",
    "rangeTo",
    "rangeUntil",
    "contains",
    "get",
    "set",
    "invoke",
    "getValue",
    "setValue",
    "provideDelegate",
    "iterator",
    "next",
    "hasNext",
    "compareTo",
    "equals",
    "inc",
    "dec",
    "unaryMinus",
    "unaryPlus",
    "not",
    "plusAssign",
    "minusAssign",
    "timesAssign",
    "divAssign",
    "remAssign",
];

/// What kind of declaration a missing name needs.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Wanted {
    /// A type name.
    Classifier,
    /// A name on its own in an expression: a class, function or property
    /// that isn't an extension.
    Value,
    /// After `receiver.`: an extension the receiver can use.
    Extension(Ty),
}

pub(super) fn diagnostics(
    db: &RootDatabase,
    file: SourceFile,
    parse: &Parse,
    config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    let checks_names = config.runs(Check::MissingImport) || config.runs(Check::UnresolvedReference);
    if checks_names {
        let imports = ImportPlacement::of(parse);
        for (range, name, wanted) in missing_names(db, file, parse) {
            found.extend(missing_name(
                db, file, &imports, range, name, wanted, config,
            ));
        }
    }
    if config.runs(Check::UnusedImport) {
        found.extend(unused_imports(parse));
    }
    found
}

/// The names in the file nothing resolves, with certainty: not one an
/// explicit import names (that import is what's broken), and none at all
/// when a star import doesn't resolve (anything could come from it).
fn missing_names(
    db: &RootDatabase,
    file: SourceFile,
    parse: &Parse,
) -> Vec<(TextRange, Name, Wanted)> {
    let tree = hir::file_item_tree(db, file);
    let star_import_unresolved = tree.imports.iter().any(|import| {
        import.target == hir::ImportTarget::Everything
            && matches!(
                hir::resolve_import_path(db, file, &import.path),
                Resolution::Unresolved
            )
    });
    if star_import_unresolved {
        return Vec::new();
    }
    let imported: FxHashSet<Name> = tree
        .imports
        .iter()
        .filter_map(|import| import.visible_name())
        .collect();
    let mut missing = Vec::new();
    let inference = hir::infer_file(db, file);
    for unresolved in inference.unresolved() {
        if unresolved.certainty != Certainty::Certain {
            continue;
        }
        let wanted = match &unresolved.site {
            NameSite::Scope => Wanted::Value,
            NameSite::Member(receiver) => Wanted::Extension(receiver.clone()),
            NameSite::Static | NameSite::Package => continue,
        };
        missing.push((unresolved.range, unresolved.name, wanted));
    }
    let local_type_names = local_type_names(parse);
    for user_type in syntax::descendants(parse.root()).filter(|node| node.kind() == "user_type") {
        let segments: Vec<SyntaxNode<'_>> = named_children(user_type)
            .filter(|child| child.kind() == "type_identifier")
            .collect();
        let [only] = segments[..] else {
            continue;
        };
        let name = Name::new(parse.text_of(only));
        if local_type_names.contains(&name) {
            continue;
        }
        let resolution = hir::resolve_type_path(db, file, &Path::new([name]), only.start_byte());
        if resolution == Resolution::Unresolved {
            missing.push((TextRange::of(only), name, Wanted::Classifier));
        }
    }
    missing.retain(|(_, name, _)| !imported.contains(name));
    missing
}

/// Names of classes, type aliases and type parameters declared anywhere
/// in the file — local ones included, which the item tree doesn't hold.
fn local_type_names(parse: &Parse) -> FxHashSet<Name> {
    syntax::descendants(parse.root())
        .filter(|node| {
            matches!(
                node.kind(),
                "class_declaration" | "object_declaration" | "type_alias" | "type_parameter"
            )
        })
        .filter_map(|node| named_children(node).find(|child| child.kind() == "type_identifier"))
        .map(|name| Name::new(parse.text_of(name)))
        .collect()
}

fn missing_name(
    db: &RootDatabase,
    file: SourceFile,
    imports: &ImportPlacement,
    range: TextRange,
    name: Name,
    wanted: Wanted,
    config: &DiagnosticsConfig,
) -> Option<Diagnostic> {
    let candidates = importable(db, file, name, &wanted);
    if candidates.is_empty() {
        if !config.runs(Check::UnresolvedReference) {
            return None;
        }
        return Some(Diagnostic {
            check: Check::UnresolvedReference,
            range,
            message: format!("Unresolved reference `{name}`"),
            fixes: Vec::new(),
        });
    }
    if !config.runs(Check::MissingImport) {
        return None;
    }
    let fixes = candidates
        .iter()
        .map(|qualified| Fix {
            label: format!("Import `{qualified}`"),
            edits: vec![imports.edit(qualified)],
        })
        .collect();
    let message = match &candidates[..] {
        [only] => format!("`{name}` isn't imported: `import {only}`"),
        several => format!(
            "`{name}` isn't imported; it's in {} packages",
            several.len()
        ),
    };
    Some(Diagnostic {
        check: Check::MissingImport,
        range,
        message,
        fixes,
    })
}

/// The qualified names an import could bring `name` in by.
fn importable(db: &RootDatabase, file: SourceFile, name: Name, wanted: &Wanted) -> Vec<String> {
    let own_package = hir::file_item_tree(db, file).package.clone();
    let mut qualified = Vec::new();
    for package in hir::packages_declaring(db, file, name) {
        if package == own_package {
            continue;
        }
        let declarations = hir::visible_top_level(db, file, &package, name);
        let fits = declarations
            .iter()
            .any(|declaration| matches(db, file, *declaration, wanted));
        if fits {
            qualified.push(package.join(name).to_string());
        }
    }
    qualified.sort();
    qualified.dedup();
    qualified
}

fn matches(
    db: &RootDatabase,
    file: SourceFile,
    declaration: ItemLocation,
    wanted: &Wanted,
) -> bool {
    let tree = hir::item_tree(db, declaration.container);
    let item = tree.item(declaration.item);
    let is_extension = matches!(
        &item.signature,
        Signature::Function {
            receiver: Some(_),
            ..
        } | Signature::Property {
            receiver: Some(_),
            ..
        }
    );
    match wanted {
        Wanted::Classifier => item.kind.is_classifier(),
        Wanted::Value => {
            let is_value = matches!(item.kind, ItemKind::Function | ItemKind::Property { .. })
                || item.kind.is_classifier();
            is_value && !is_extension
        }
        Wanted::Extension(receiver) => {
            is_extension && hir::extends(db, file, declaration, receiver)
        }
    }
}

/// Where a new import goes.
pub(crate) struct ImportPlacement {
    offset: usize,
    /// Blank line needed before (after a `package` line, no imports yet).
    after_package: bool,
}

impl ImportPlacement {
    pub(crate) fn of(parse: &Parse) -> ImportPlacement {
        let root = parse.root();
        let last_import = syntax::descendants(root)
            .filter(|node| node.kind() == "import_header")
            .last();
        if let Some(import) = last_import {
            return ImportPlacement {
                offset: line_end(parse.text(), import.end_byte()),
                after_package: false,
            };
        }
        match named_children(root).find(|child| child.kind() == "package_header") {
            Some(package) => ImportPlacement {
                offset: line_end(parse.text(), package.end_byte()),
                after_package: true,
            },
            None => ImportPlacement {
                offset: 0,
                after_package: false,
            },
        }
    }

    pub(crate) fn edit(&self, qualified: &str) -> TextEdit {
        let new_text = if self.after_package {
            format!("\nimport {qualified}\n")
        } else if self.offset == 0 {
            format!("import {qualified}\n\n")
        } else {
            format!("import {qualified}\n")
        };
        TextEdit {
            range: TextRange::new(self.offset, self.offset),
            new_text,
        }
    }
}

/// The offset just after the line `offset` is on.
fn line_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .find('\n')
        .map(|newline| offset + newline + 1)
        .unwrap_or(text.len())
}

/// Imports whose name appears nowhere else in the file (not in code, not in
/// comments) and isn't a convention's.
fn unused_imports(parse: &Parse) -> Vec<Diagnostic> {
    let text = parse.text();
    let root = parse.root();
    let mut used: FxHashSet<&str> = FxHashSet::default();
    // The package line and the imports (aliases included) aren't uses.
    let is_header =
        |node: SyntaxNode<'_>| matches!(node.kind(), "import_header" | "package_header");
    for node in syntax::descendants_pruned(root, is_header) {
        match node.kind() {
            "simple_identifier" | "type_identifier" | "interpolated_identifier" => {
                used.insert(parse.text_of(node).trim_matches('`'));
            }
            kind if kind.contains("comment") => {
                used.extend(
                    parse
                        .text_of(node)
                        .split(|char: char| !char.is_alphanumeric() && char != '_'),
                );
            }
            _ => {}
        }
    }
    let mut unused = Vec::new();
    for import in syntax::descendants(root).filter(|node| node.kind() == "import_header") {
        if named_children(import).any(|child| child.kind() == "wildcard_import")
            || has_token(import, "*")
        {
            continue;
        }
        let alias = named_children(import)
            .find(|child| child.kind() == "import_alias")
            .and_then(|alias| named_children(alias).next());
        let last_segment = named_children(import)
            .find(|child| child.kind() == "identifier")
            .and_then(|path| named_children(path).last());
        let Some(visible) = alias.or(last_segment) else {
            continue;
        };
        let name = parse.text_of(visible).trim_matches('`');
        let is_convention = CONVENTION_NAMES.contains(&name) || name.starts_with("component");
        if is_convention || used.contains(name) {
            continue;
        }
        let start = text[..import.start_byte()]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let end = line_end(text, import.end_byte());
        unused.push(Diagnostic {
            check: Check::UnusedImport,
            range: TextRange::of(import),
            message: format!("Unused import `{name}`"),
            fixes: vec![Fix {
                label: "Remove the unused import".to_owned(),
                edits: vec![TextEdit {
                    range: TextRange::new(start, end),
                    new_text: String::new(),
                }],
            }],
        });
    }
    unused
}
