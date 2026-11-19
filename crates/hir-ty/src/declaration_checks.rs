//! A file's classes against their supertypes, as the compiler checks them:
//! an `override` with nothing to override (`NOTHING_TO_OVERRIDE`), a member
//! that overrides an inherited one without saying `override`
//! (`VIRTUAL_MEMBER_HIDDEN`), a class that leaves an inherited abstract
//! member unimplemented (`ABSTRACT_MEMBER_NOT_IMPLEMENTED`). Only for a
//! class whose whole hierarchy resolves, a member whose types all do, and
//! a whole classpath: otherwise what it overrides isn't all known.

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ClassKind, Item, ItemKind, Modifiers, Visibility};
use hir_def::{ItemContainer, ItemLocation, file_item_tree, item_source_map, item_tree};
use intern::Name;
use rustc_hash::FxHashSet;
use syntax::TextRange;

use crate::members::{Types, is_java};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeclarationFinding {
    /// `override` on a member no supertype has one like: where it's named.
    NothingToOverride { name: TextRange, member: Name },
    /// A member with an inherited one's signature that doesn't say
    /// `override`: where it's named, and the class it hides a member of.
    HiddenMember {
        name: TextRange,
        member: Name,
        owner: Name,
    },
    /// A class that isn't abstract and implements nowhere an abstract
    /// member it inherits: where the class is named, and the member's
    /// class.
    NotImplemented {
        name: TextRange,
        class: Name,
        member: Name,
        owner: Name,
    },
}

/// What `file`'s classes declare against their supertypes.
pub fn declaration_findings(db: &dyn SourceDatabase, file: SourceFile) -> Vec<DeclarationFinding> {
    // A partial classpath may miss a supertype's members, `Any`'s too; a
    // platform but the JVM has members of its own (`Any.toString` of
    // Kotlin/Native's).
    let judged = file.language(db) == syntax::Language::Kotlin
        && hir_def::def_map::classpath_complete(db, file)
        && hir_def::def_map::sees_jdk_from(db, file);
    if !judged {
        return Vec::new();
    }
    let tree = file_item_tree(db, file);
    let source_map = item_source_map(db, ItemContainer::File(file));
    let types = Types::new(db, file);
    let mut found = Vec::new();
    for (index, item) in tree.indexed_items() {
        let ItemKind::Class(kind) = item.kind else {
            continue;
        };
        let class = ItemLocation::in_file(file, index);
        if is_multiplatform(item)
            || !types.hierarchy_complete(class)
            || has_multiplatform_supertype(db, &types, class)
        {
            continue;
        }
        let inherited = inherited_names(db, &types, class);
        for child in item.children.iter() {
            let member_item = tree.item(*child);
            // A member that doesn't say `override` and whose name nothing
            // inherited has can't hide anything: most of them.
            let may_hide = inherited.contains(&member_item.name);
            if !may_hide && !member_item.modifiers.contains(Modifiers::OVERRIDE) {
                continue;
            }
            let member = ItemLocation::in_file(file, *child);
            let name = source_map.ranges(*child).name_range;
            found.extend(member_finding(db, &types, member, name));
        }
        if must_implement(kind, item) {
            let missing = types.unimplemented_members(class).into_iter().next();
            if let Some(missing) = missing {
                found.push(DeclarationFinding::NotImplemented {
                    name: source_map.ranges(index).name_range,
                    class: item.name,
                    member: item_tree(db, missing.container).item(missing.item).name,
                    owner: owner_name(db, missing),
                });
            }
        }
    }
    found
}

/// The names of the members `class`'s supertypes declare.
fn inherited_names(
    db: &dyn SourceDatabase,
    types: &Types<'_>,
    class: ItemLocation,
) -> FxHashSet<Name> {
    let mut names = FxHashSet::default();
    for supertype in types.all_supertypes(&types.self_type(class)).iter().skip(1) {
        let tree = item_tree(db, supertype.class.container);
        for child in tree.item(supertype.class.item).children.iter() {
            names.insert(tree.item(*child).name);
        }
    }
    names
}

/// What's wrong with how `member` overrides, if anything.
fn member_finding(
    db: &dyn SourceDatabase,
    types: &Types<'_>,
    member: ItemLocation,
    name: TextRange,
) -> Option<DeclarationFinding> {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    let is_member = matches!(item.kind, ItemKind::Function | ItemKind::Property { .. });
    if !is_member || is_multiplatform(item) || !types.signature_known(member) {
        return None;
    }
    let inherited: Vec<ItemLocation> = types
        .overridden(member)
        .into_iter()
        .filter(|overridden| may_be_overridden(db, *overridden))
        .collect();
    let says_override = item.modifiers.contains(Modifiers::OVERRIDE);
    match (says_override, inherited.first()) {
        (true, None)
            if !overrides_java_accessor(db, types, member)
                && !types.may_override_unknown(member)
                && !member_class(db, member)
                    .is_some_and(|class| extends_mapped_built_in(db, types, class)) =>
        {
            Some(DeclarationFinding::NothingToOverride {
                name,
                member: item.name,
            })
        }
        (false, Some(hidden)) if item.visibility != Visibility::Private => {
            Some(DeclarationFinding::HiddenMember {
                name,
                member: item.name,
                owner: owner_name(db, *hidden),
            })
        }
        _ => None,
    }
}

/// The class `member` is declared in.
fn member_class(db: &dyn SourceDatabase, member: ItemLocation) -> Option<ItemLocation> {
    let parent = item_tree(db, member.container).item(member.item).parent?;
    Some(ItemLocation {
        container: member.container,
        item: parent,
    })
}

/// The name of the class `member` is declared in.
fn owner_name(db: &dyn SourceDatabase, member: ItemLocation) -> Name {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    match item.parent {
        Some(parent) => tree.item(parent).name,
        None => item.name,
    }
}

/// Whether a member another one would override is one a class can
/// override: not private, not static.
fn may_be_overridden(db: &dyn SourceDatabase, member: ItemLocation) -> bool {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    item.visibility != Visibility::Private && !item.modifiers.contains(Modifiers::STATIC)
}

/// `override val size` of a class extending a Java collection: Kotlin
/// sees its `size()` (or a getter, `getName()`) as that property.
fn overrides_java_accessor(
    db: &dyn SourceDatabase,
    types: &Types<'_>,
    member: ItemLocation,
) -> bool {
    let tree = item_tree(db, member.container);
    let item = tree.item(member.item);
    let (ItemKind::Property { .. }, Some(parent)) = (item.kind, item.parent) else {
        return false;
    };
    let class = ItemLocation {
        container: member.container,
        item: parent,
    };
    let name = item.name.as_str();
    types
        .all_supertypes(&types.self_type(class))
        .iter()
        .filter(|supertype| is_java(db, supertype.class))
        .any(|supertype| {
            let supertype_tree = item_tree(db, supertype.class.container);
            supertype_tree
                .item(supertype.class.item)
                .children
                .iter()
                .any(|child| {
                    let location = ItemLocation {
                        container: supertype.class.container,
                        item: *child,
                    };
                    let accessor = supertype_tree.item(*child);
                    let named_so = accessor.name.as_str() == name
                        || types.java_property_name(location).as_deref() == Some(name);
                    accessor.kind == ItemKind::Function && named_so
                })
        })
}

/// A class that must implement what it inherits: not abstract, sealed,
/// an interface, an enum (whose entries may) or an annotation, and not
/// delegating a supertype `by` another object.
fn must_implement(kind: ClassKind, item: &Item) -> bool {
    let concrete_kind = matches!(
        kind,
        ClassKind::Class | ClassKind::Object | ClassKind::CompanionObject
    );
    let modifiers = item.modifiers;
    let abstract_or_open_ended = modifiers.contains(Modifiers::ABSTRACT)
        || modifiers.contains(Modifiers::SEALED)
        || modifiers.contains(Modifiers::DELEGATES);
    concrete_kind && !abstract_or_open_ended
}

/// Whether a class extends an `expect` or `actual` one: the `expect` the
/// hierarchy finds may lack what an `actual` declares.
fn has_multiplatform_supertype(
    db: &dyn SourceDatabase,
    types: &Types<'_>,
    class: ItemLocation,
) -> bool {
    types
        .all_supertypes(&types.self_type(class))
        .iter()
        .any(|supertype| {
            is_multiplatform(item_tree(db, supertype.class.container).item(supertype.class.item))
        })
}

/// A class whose hierarchy has a built-in mapped to a JDK class (a
/// collection, `Throwable`, `CharSequence`): it may override the JDK
/// class's members Kotlin doesn't show (`forEach(Consumer)` of
/// `java.lang.Iterable`).
fn extends_mapped_built_in(
    db: &dyn SourceDatabase,
    types: &Types<'_>,
    class: ItemLocation,
) -> bool {
    types
        .all_supertypes(&types.self_type(class))
        .iter()
        .skip(1)
        .any(|supertype| {
            let tree = item_tree(db, supertype.class.container);
            let package = tree.package.to_string();
            let name = tree.item(supertype.class.item).name;
            package == "kotlin.collections" || package == "kotlin" && name.as_str() != "Any"
        })
}

/// An `expect` or `actual` declaration: its counterparts complete it.
fn is_multiplatform(item: &Item) -> bool {
    item.modifiers.contains(Modifiers::EXPECT) || item.modifiers.contains(Modifiers::ACTUAL)
}
