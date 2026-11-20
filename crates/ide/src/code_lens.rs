//! `textDocument/codeLens` for Kotlin files: over each declaration, how
//! many references it has; over each member that overrides others, what it
//! overrides; over each class and member that can be implemented, its
//! implementations (each found when the editor asks, see
//! `codeLens/resolve`); over each test function and test class, a run of
//! it by the build, as a Gradle test filter.

use hir::{
    ClassKind, FileId, ItemId, ItemKind, ItemTree, Language, Modifiers, Path, Resolution,
    RootDatabase, SourceDatabase, SourceFile, Visibility,
};
use syntax::TextRange;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CodeLens {
    /// The declaration's name.
    pub range: TextRange,
    pub kind: CodeLensKind,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CodeLensKind {
    /// The references to the declaration named at `range`, to count.
    References,
    /// What the member named at `range` overrides, to find (↑).
    Overridden,
    /// What implements the class or member named at `range`, to find (↓).
    Implementations,
    /// Running a test function, or every test in a class.
    RunTests { target: TestTarget, filter: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TestTarget {
    Function,
    Class,
}

/// The annotations that make a function a test, for JUnit 4 and 5,
/// kotlin.test and TestNG.
const TEST_ANNOTATIONS: &[&str] = &[
    "org.junit.Test",
    "org.junit.jupiter.api.Test",
    "org.junit.jupiter.api.RepeatedTest",
    "org.junit.jupiter.api.TestFactory",
    "org.junit.jupiter.api.TestTemplate",
    "org.junit.jupiter.params.ParameterizedTest",
    "kotlin.test.Test",
    "org.testng.annotations.Test",
];

pub(crate) fn code_lenses(db: &RootDatabase, file: FileId) -> Vec<CodeLens> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    if hir::parse(db, source).language() != Language::Kotlin {
        return Vec::new();
    }
    let tree = hir::file_item_tree(db, source);
    let source_map = hir::file_item_source_map(db, source);
    let tests = Tests::of(db, source, &tree);
    let mut lenses = Vec::new();
    for (item_id, item) in tree.indexed_items() {
        let counted = matches!(
            item.kind,
            ItemKind::Class(_)
                | ItemKind::Function
                | ItemKind::Property { .. }
                | ItemKind::TypeAlias
        );
        if !counted {
            continue;
        }
        let range = source_map.ranges(item_id).name_range;
        let overrides = item.modifiers.contains(Modifiers::OVERRIDE);
        if overrides {
            lenses.push(CodeLens {
                range,
                kind: CodeLensKind::Overridden,
            });
        }
        if can_be_implemented(&tree, item_id) {
            lenses.push(CodeLens {
                range,
                kind: CodeLensKind::Implementations,
            });
        }
        if let Some(target) = tests.target(item_id) {
            lenses.push(CodeLens {
                range,
                kind: CodeLensKind::RunTests {
                    target,
                    filter: test_filter(&tree, item_id),
                },
            });
        }
        lenses.push(CodeLens {
            range,
            kind: CodeLensKind::References,
        });
    }
    lenses
}

/// Whether a class or member may have implementations: an interface, an
/// abstract, open or sealed class; a member of one of those that is
/// abstract, open, an interface's, or an override not made final. Private
/// ones can't be overridden.
fn can_be_implemented(tree: &ItemTree, item_id: ItemId) -> bool {
    let item = tree.item(item_id);
    if item.visibility == Visibility::Private {
        return false;
    }
    match item.kind {
        ItemKind::Class(_) => is_inheritable(tree, item_id),
        ItemKind::Function | ItemKind::Property { .. } => {
            let Some(class) = item.parent else {
                return false;
            };
            let in_interface = tree.item(class).kind == ItemKind::Class(ClassKind::Interface);
            let declared_open = [Modifiers::ABSTRACT, Modifiers::OPEN]
                .into_iter()
                .any(|modifier| item.modifiers.contains(modifier));
            let open_override = item.modifiers.contains(Modifiers::OVERRIDE)
                && !item.modifiers.contains(Modifiers::FINAL);
            is_inheritable(tree, class) && (in_interface || declared_open || open_override)
        }
        _ => false,
    }
}

/// Whether a class can be extended: an interface, or an abstract, open or
/// sealed class.
fn is_inheritable(tree: &ItemTree, class: ItemId) -> bool {
    let item = tree.item(class);
    match item.kind {
        ItemKind::Class(ClassKind::Interface) => true,
        ItemKind::Class(ClassKind::Class) => {
            [Modifiers::ABSTRACT, Modifiers::OPEN, Modifiers::SEALED]
                .into_iter()
                .any(|modifier| item.modifiers.contains(modifier))
        }
        _ => false,
    }
}

/// Which of a file's items are tests: functions with a test annotation, and
/// classes holding one (in themselves or a nested class).
struct Tests {
    functions: Vec<ItemId>,
    classes: Vec<ItemId>,
}

impl Tests {
    fn of(db: &RootDatabase, source: SourceFile, tree: &ItemTree) -> Tests {
        let functions: Vec<ItemId> = tree
            .indexed_items()
            .filter(|(_, item)| item.kind == ItemKind::Function && item.parent.is_some())
            .filter(|(item_id, _)| {
                tree.annotations(*item_id)
                    .iter()
                    .any(|path| is_test_annotation(db, source, tree, path, *item_id))
            })
            .map(|(item_id, _)| item_id)
            .collect();
        let mut classes: Vec<ItemId> = Vec::new();
        for function in &functions {
            let mut enclosing = tree.item(*function).parent;
            while let Some(class) = enclosing {
                if !classes.contains(&class) {
                    classes.push(class);
                }
                enclosing = tree.item(class).parent;
            }
        }
        Tests { functions, classes }
    }

    fn target(&self, item: ItemId) -> Option<TestTarget> {
        if self.functions.contains(&item) {
            Some(TestTarget::Function)
        } else if self.classes.contains(&item) {
            Some(TestTarget::Class)
        } else {
            None
        }
    }
}

/// Whether the annotation written as `path` on `item` is a test one: by
/// what it resolves to, or — without the test library on the classpath —
/// by its import or its qualified spelling.
fn is_test_annotation(
    db: &RootDatabase,
    source: SourceFile,
    tree: &ItemTree,
    path: &Path,
    item: ItemId,
) -> bool {
    let qualified = match hir::resolve_type_path_in_declaration(db, source, path, item) {
        Resolution::Declaration(class) => Some(qualified_name(db, class)),
        Resolution::Unresolved => written_qualified_name(tree, path),
        _ => None,
    };
    qualified.is_some_and(|name| TEST_ANNOTATIONS.contains(&name.as_str()))
}

/// `package.Outer.Name` of a class.
fn qualified_name(db: &RootDatabase, class: hir::ItemLocation) -> String {
    let tree = hir::item_tree(db, class.container);
    let mut names = vec![tree.item(class.item).name.to_string()];
    let mut enclosing = tree.item(class.item).parent;
    while let Some(outer) = enclosing {
        names.push(tree.item(outer).name.to_string());
        enclosing = tree.item(outer).parent;
    }
    if !tree.package.is_root() {
        names.push(tree.package.to_string());
    }
    names.reverse();
    names.join(".")
}

/// The qualified name an unresolved annotation is written with: as it is
/// when qualified, else from the import of its name.
fn written_qualified_name(tree: &ItemTree, path: &Path) -> Option<String> {
    if path.segments().len() > 1 {
        return Some(path.to_string());
    }
    let name = path.last()?;
    tree.imports
        .iter()
        .find(|import| import.visible_name() == Some(name))
        .map(|import| import.path.to_string())
}

/// Gradle's `--tests` filter for a test class (`package.Outer$Nested`) or
/// function (`package.Class.function`).
fn test_filter(tree: &ItemTree, item: ItemId) -> String {
    let is_function = tree.item(item).kind == ItemKind::Function;
    let class = if is_function {
        tree.item(item).parent
    } else {
        Some(item)
    };
    let mut classes = Vec::new();
    let mut enclosing = class;
    while let Some(current) = enclosing {
        classes.push(tree.item(current).name.to_string());
        enclosing = tree.item(current).parent;
    }
    classes.reverse();
    let mut filter = classes.join("$");
    if !tree.package.is_root() {
        filter = format!("{}.{filter}", tree.package);
    }
    if is_function {
        filter = format!("{filter}.{}", tree.item(item).name);
    }
    filter
}
