//! Kotlin Multiplatform's `expect` declarations and their `actual`s, which
//! are one declaration seen from different platforms: references and
//! renaming take them together, "go to implementation" leads from the
//! `expect` to the `actual`s.

use base_db::{SourceDatabase, SourceFile};
use intern::{Name, Path};

use crate::item_tree::{ItemKind, ItemTree, Modifiers, Signature, type_text};
use crate::{ItemContainer, ItemId, ItemLocation, file_item_tree, item_tree};

/// Which side of the pair a declaration is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// `expect`, or inside an `expect` class.
    Expect,
    /// `actual`, or inside an `actual` class.
    Actual,
    /// Neither: an ordinary declaration.
    Neither,
}

/// What must agree between an `expect` and an `actual`.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Shape {
    package: Path,
    /// The enclosing classes' names, outermost first, then the name.
    names: Vec<Name>,
    kind: KindGroup,
    /// For a function: its receiver's and parameters' types, by the last
    /// segment of each as written (`List<T>` → `List`), so overloads stay
    /// apart however their types are imported.
    parameters: Option<Vec<String>>,
}

/// An `expect class` may be `actual` as a class or a type alias, and an
/// `expect val` as a `var`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KindGroup {
    Classifier,
    Function,
    Property,
    Constructor,
    EnumEntry,
}

impl KindGroup {
    fn of(kind: ItemKind) -> KindGroup {
        match kind {
            ItemKind::Class(_) | ItemKind::TypeAlias => KindGroup::Classifier,
            ItemKind::Function => KindGroup::Function,
            ItemKind::Property { .. } => KindGroup::Property,
            ItemKind::Constructor => KindGroup::Constructor,
            ItemKind::EnumEntry => KindGroup::EnumEntry,
        }
    }
}

/// Whether `declaration` is an `expect` or `actual` declaration, itself or
/// by an enclosing class.
pub fn side_of(db: &dyn SourceDatabase, declaration: ItemLocation) -> Side {
    side(&item_tree(db, declaration.container), declaration.item)
}

fn side(tree: &ItemTree, item: ItemId) -> Side {
    let mut chain = vec![item];
    chain.extend(tree.ancestors(item));
    for link in chain {
        let modifiers = tree.item(link).modifiers;
        if modifiers.contains(Modifiers::EXPECT) {
            return Side::Expect;
        }
        if modifiers.contains(Modifiers::ACTUAL) {
            return Side::Actual;
        }
    }
    Side::Neither
}

/// `declaration` and the `expect` or `actual` declarations it pairs with
/// in its module (same package, nesting, name, kind and parameter types),
/// itself first. Just `declaration` when it's neither `expect` nor `actual`.
pub fn expect_actual_family(
    db: &dyn SourceDatabase,
    declaration: ItemLocation,
) -> Vec<ItemLocation> {
    let ItemContainer::File(file) = declaration.container else {
        return vec![declaration];
    };
    let tree = item_tree(db, declaration.container);
    if side(&tree, declaration.item) == Side::Neither {
        return vec![declaration];
    }
    let shape = shape_of(&tree, declaration.item);
    let mut family = vec![declaration];
    for candidate_file in module_files(db, file) {
        let candidate_tree = file_item_tree(db, candidate_file);
        if candidate_tree.package != shape.package {
            continue;
        }
        for candidate in items_named(&candidate_tree, &shape.names) {
            let location = ItemLocation::in_file(candidate_file, candidate);
            let pairs = side(&candidate_tree, candidate) != Side::Neither
                && shape_of(&candidate_tree, candidate) == shape;
            if pairs && !family.contains(&location) {
                family.push(location);
            }
        }
    }
    family
}

/// Drops the `expect` declarations among `candidates` that one of them on
/// the `actual` side pairs with: where both are visible (platform code sees
/// its common code), a name means the `actual`.
pub fn hide_expects_with_actuals(db: &dyn SourceDatabase, candidates: &mut Vec<ItemLocation>) {
    if candidates.len() < 2 {
        return;
    }
    let sides: Vec<Side> = candidates
        .iter()
        .map(|candidate| side_of(db, *candidate))
        .collect();
    let has_both = sides.contains(&Side::Expect) && sides.contains(&Side::Actual);
    if !has_both {
        return;
    }
    let shape = |location: ItemLocation| {
        let tree = item_tree(db, location.container);
        shape_of(&tree, location.item)
    };
    let actual_shapes: Vec<Shape> = candidates
        .iter()
        .zip(&sides)
        .filter(|(_, side)| **side == Side::Actual)
        .map(|(candidate, _)| shape(*candidate))
        .collect();
    let mut index = 0;
    candidates.retain(|candidate| {
        let hidden = sides[index] == Side::Expect && actual_shapes.contains(&shape(*candidate));
        index += 1;
        !hidden
    });
}

fn shape_of(tree: &ItemTree, item: ItemId) -> Shape {
    let mut names: Vec<Name> = tree
        .ancestors(item)
        .iter()
        .rev()
        .map(|ancestor| tree.item(*ancestor).name)
        .collect();
    let declared = tree.item(item);
    names.push(declared.name);
    let parameters = match &declared.signature {
        Signature::Function {
            receiver,
            parameters,
            ..
        } => Some(
            receiver
                .iter()
                .chain(
                    parameters
                        .iter()
                        .filter_map(|parameter| parameter.type_ref.as_ref()),
                )
                .map(|type_ref| last_segment(&type_text(type_ref)))
                .collect(),
        ),
        _ => None,
    };
    Shape {
        package: tree.package.clone(),
        names,
        kind: KindGroup::of(declared.kind),
        parameters,
    }
}

/// `a.b.List<T>?` → `List`.
fn last_segment(written: &str) -> String {
    let without_arguments = written.split('<').next().unwrap_or(written);
    let name = without_arguments
        .rsplit('.')
        .next()
        .unwrap_or(without_arguments);
    name.trim_end_matches('?').to_owned()
}

/// The items at the nesting `names` spells (outermost class first).
fn items_named(tree: &ItemTree, names: &[Name]) -> Vec<ItemId> {
    let Some((last, enclosing)) = names.split_last() else {
        return Vec::new();
    };
    let mut parents: Vec<Option<ItemId>> = vec![None];
    for name in enclosing {
        parents = parents
            .iter()
            .flat_map(|parent| tree.named(*parent, *name).iter().map(|class| Some(*class)))
            .collect();
    }
    parents
        .iter()
        .flat_map(|parent| tree.named(*parent, *last).iter().copied())
        .collect()
}

/// The workspace files of `file`'s module, every source set's.
fn module_files(db: &dyn SourceDatabase, file: SourceFile) -> Vec<SourceFile> {
    let Some(own_set) = file.source_set(db) else {
        return vec![file];
    };
    let workspace = db.workspace();
    let model = workspace.model(db);
    let module = model.source_set(own_set).module;
    workspace
        .source_sets(db)
        .iter()
        .filter(|source_set| model.source_set(source_set.id(db)).module == module)
        .flat_map(|source_set| source_set.files(db).iter().copied())
        .collect()
}
