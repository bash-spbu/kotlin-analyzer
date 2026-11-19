//! One file's declarations and their signatures, without bodies.
//!
//! An `ItemTree` is what the rest of a module needs to know about a file:
//! its package, imports, and every declaration with the types it names.
//! Function bodies and initializers aren't in it, so editing inside a body
//! changes nothing that only depends on declarations.

mod lower_java;
mod lower_kotlin;
pub(crate) mod pretty;
mod written_annotations;
mod written_contract;

use intern::{Name, Path};
use std::hash::{Hash, Hasher};

use rustc_hash::{FxHashMap, FxHashSet};
use syntax::TextRange;

pub use classfile::{
    CallsInPlace, ContractCondition, ContractEffect, ContractSubject, DeprecatedSinceKotlin,
    Deprecation, DeprecationLevel, DeprecationOrigin, Invocation, OptInLevel, OptInMarker, Returns,
};

/// A function's contract, its types as written (see [`classfile::Contract`]).
pub type Contract = classfile::Contract<TypeRef>;
pub(crate) use lower_java::lower_java;
pub use lower_kotlin::kotlin_type_ref;
pub(crate) use lower_kotlin::lower_kotlin;
pub use pretty::type_text;

/// An item's identity in its file's [`ItemTree`]: derived from its parent,
/// its kind, its name and its place among same-named siblings of that kind
/// — not from where it is in the file — so an edit elsewhere in the file
/// leaves it, and everything that refers to it, unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ItemId(u32);

/// The coarse kind an [`ItemId`] is derived from: turning a `val` into a
/// `var`, or a class into an interface, keeps the item's identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum IdentityKind {
    Classifier,
    Function,
    Property,
    Constructor,
    EnumEntry,
    TypeAlias,
}

impl IdentityKind {
    fn of(kind: &ItemKind) -> IdentityKind {
        match kind {
            ItemKind::Class(_) => IdentityKind::Classifier,
            ItemKind::Function => IdentityKind::Function,
            ItemKind::Property { .. } => IdentityKind::Property,
            ItemKind::Constructor => IdentityKind::Constructor,
            ItemKind::EnumEntry => IdentityKind::EnumEntry,
            ItemKind::TypeAlias => IdentityKind::TypeAlias,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ItemTree {
    /// The file's package; the root package when it declares none.
    pub package: Path,
    /// For a library package: the source file each top-level item was
    /// compiled from (`Strings.kt`), to find it in a sources archive.
    pub source_files: Box<[(ItemId, Box<str>)]>,
    pub imports: Box<[Import]>,
    /// Every declaration of the file, nested ones included, in document
    /// order, and each one's id.
    items: Box<[Item]>,
    ids: Box<[ItemId]>,
    positions: FxHashMap<ItemId, u32>,
    /// Items by parent (`None`: top level) and name, in document order —
    /// a library package has thousands of top-level functions, a lookup
    /// mustn't walk them.
    named: FxHashMap<(Option<ItemId>, Name), Box<[ItemId]>>,
    pub top_level: Box<[ItemId]>,
    /// What each deprecated item's deprecation says (its modifiers have
    /// `DEPRECATED`).
    deprecations: FxHashMap<ItemId, Deprecation>,
    /// The annotation classes on each annotated item that may mark it as
    /// needing opt-in: qualified for a library's items, as written for a
    /// file's (resolved where it's declared).
    annotations: FxHashMap<ItemId, Box<[Path]>>,
    /// The annotation classes that are opt-in markers (`@RequiresOptIn`).
    opt_in_markers: FxHashMap<ItemId, OptInMarker>,
    /// What each function with a contract promises (`contract { … }`).
    contracts: FxHashMap<ItemId, Contract>,
}

impl ItemTree {
    /// The function's contract, if it has one.
    pub fn contract(&self, id: ItemId) -> Option<&Contract> {
        self.contracts.get(&id)
    }

    /// What `@Deprecated` on the item says, if it's deprecated.
    pub fn deprecation(&self, id: ItemId) -> Option<&Deprecation> {
        self.deprecations.get(&id)
    }

    /// The annotation classes on the item that may mark it as needing
    /// opt-in.
    pub fn annotations(&self, id: ItemId) -> &[Path] {
        self.annotations.get(&id).map_or(&[], |paths| &paths[..])
    }

    /// `@RequiresOptIn` on an annotation class.
    pub fn opt_in_marker(&self, id: ItemId) -> Option<&OptInMarker> {
        self.opt_in_markers.get(&id)
    }

    pub fn item(&self, id: ItemId) -> &Item {
        &self.items[self.positions[&id] as usize]
    }

    /// The items named `name` declared in `parent` (`None`: at the top
    /// level), in document order.
    pub fn named(&self, parent: Option<ItemId>, name: Name) -> &[ItemId] {
        self.named
            .get(&(parent, name))
            .map(|items| &items[..])
            .unwrap_or_default()
    }

    /// Every item with its id, in document order.
    pub fn indexed_items(&self) -> impl Iterator<Item = (ItemId, &Item)> {
        self.ids.iter().copied().zip(self.items.iter())
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The items `index` is declared in, outermost first — the scope its
    /// header sees (its own body is not around its header).
    pub fn ancestors(&self, index: ItemId) -> Vec<ItemId> {
        let mut ancestors = Vec::new();
        let mut current = self.item(index).parent;
        while let Some(parent) = current {
            ancestors.push(parent);
            current = self.item(parent).parent;
        }
        ancestors.reverse();
        ancestors
    }

    /// One declaration as Kotlin-like text: `data class Point<T>(x: T) : Shape`.
    pub fn signature(&self, index: ItemId) -> String {
        pretty::header(self.item(index))
    }

    /// A Java declaration's signature as Java writes it (`static String
    /// join(String separator, String... parts)`), with simple type names;
    /// `parameter_names`, when given (from its source), instead of the
    /// ones a class file may lack (`p0`, `p1`).
    pub fn java_signature(&self, index: ItemId, parameter_names: Option<&[String]>) -> String {
        pretty::java_header(self, index, parameter_names)
    }

    /// The tree as text, for snapshot tests and debugging.
    pub fn pretty(&self) -> String {
        pretty::print(self)
    }
}

/// An `import` line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Import {
    /// `a.b.C` of `import a.b.C`; `a.b` of `import a.b.*`.
    pub path: Path,
    pub target: ImportTarget,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ImportTarget {
    /// `import a.b.C` (`as Alias`).
    Single { alias: Option<Name> },
    /// `import a.b.*`.
    Everything,
}

impl Import {
    /// The name the import makes visible, unless it imports everything.
    pub fn visible_name(&self) -> Option<Name> {
        match self.target {
            ImportTarget::Single { alias } => alias.or_else(|| self.path.last()),
            ImportTarget::Everything => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pub name: Name,
    pub kind: ItemKind,
    pub parent: Option<ItemId>,
    pub children: Box<[ItemId]>,
    pub visibility: Visibility,
    pub modifiers: Modifiers,
    pub type_parameters: Box<[TypeParameter]>,
    pub signature: Signature,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ItemKind {
    Class(ClassKind),
    Function,
    Property { mutable: bool },
    TypeAlias,
    EnumEntry,
    Constructor,
}

impl ItemKind {
    /// A type a value can have, or that holds members: classes, interfaces,
    /// objects, enums, annotations — and type aliases standing for one.
    pub fn is_classifier(self) -> bool {
        matches!(self, ItemKind::Class(_) | ItemKind::TypeAlias)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClassKind {
    Class,
    Interface,
    Enum,
    Annotation,
    Object,
    CompanionObject,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Visibility {
    Public,
    Protected,
    Internal,
    Private,
    /// Java's default: the package only.
    PackagePrivate,
}

/// Declaration modifiers that change what a declaration means.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u32);

impl Modifiers {
    pub const DATA: Modifiers = Modifiers(1 << 0);
    pub const SEALED: Modifiers = Modifiers(1 << 1);
    pub const ABSTRACT: Modifiers = Modifiers(1 << 2);
    pub const OPEN: Modifiers = Modifiers(1 << 3);
    pub const INNER: Modifiers = Modifiers(1 << 4);
    pub const VALUE: Modifiers = Modifiers(1 << 5);
    pub const FUN_INTERFACE: Modifiers = Modifiers(1 << 6);
    pub const EXPECT: Modifiers = Modifiers(1 << 7);
    pub const ACTUAL: Modifiers = Modifiers(1 << 8);
    pub const OVERRIDE: Modifiers = Modifiers(1 << 9);
    pub const SUSPEND: Modifiers = Modifiers(1 << 10);
    pub const INLINE: Modifiers = Modifiers(1 << 11);
    pub const OPERATOR: Modifiers = Modifiers(1 << 12);
    pub const INFIX: Modifiers = Modifiers(1 << 13);
    pub const CONST: Modifiers = Modifiers(1 << 14);
    pub const LATEINIT: Modifiers = Modifiers(1 << 15);
    /// Java `static`.
    pub const STATIC: Modifiers = Modifiers(1 << 16);
    /// Java `final` on a class or method.
    pub const FINAL: Modifiers = Modifiers(1 << 17);
    pub const DEPRECATED: Modifiers = Modifiers(1 << 18);
    /// Compiled from Java: its types' nullability is unknown to Kotlin.
    pub const PLATFORM: Modifiers = Modifiers(1 << 19);
    /// Generated by the compiler (a compiled data class's `componentN`,
    /// `copy`, …), not written. Not a modifier Kotlin writes: signatures
    /// don't show it.
    pub const SYNTHESIZED: Modifiers = Modifiers(1 << 20);
    /// A property that writes its getter: not a stored value smart casts
    /// can rely on. Not a modifier Kotlin writes.
    pub const GETTER: Modifiers = Modifiers(1 << 21);
    /// A property delegated `by` another object. Not a modifier Kotlin
    /// writes.
    pub const DELEGATED: Modifiers = Modifiers(1 << 22);
    /// A class implementing a supertype `by` a delegate (`: Foo by foo`):
    /// the delegate implements its members. Not a modifier Kotlin writes.
    pub const DELEGATES: Modifiers = Modifiers(1 << 23);

    const NAMES: [(Modifiers, &'static str); 20] = [
        (Modifiers::DATA, "data"),
        (Modifiers::SEALED, "sealed"),
        (Modifiers::ABSTRACT, "abstract"),
        (Modifiers::OPEN, "open"),
        (Modifiers::INNER, "inner"),
        (Modifiers::VALUE, "value"),
        (Modifiers::FUN_INTERFACE, "fun"),
        (Modifiers::EXPECT, "expect"),
        (Modifiers::ACTUAL, "actual"),
        (Modifiers::OVERRIDE, "override"),
        (Modifiers::SUSPEND, "suspend"),
        (Modifiers::INLINE, "inline"),
        (Modifiers::OPERATOR, "operator"),
        (Modifiers::INFIX, "infix"),
        (Modifiers::CONST, "const"),
        (Modifiers::LATEINIT, "lateinit"),
        (Modifiers::STATIC, "static"),
        (Modifiers::FINAL, "final"),
        (Modifiers::DEPRECATED, "@Deprecated"),
        (Modifiers::PLATFORM, "(java)"),
    ];

    pub fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Modifiers) {
        self.0 |= other.0;
    }

    /// The modifier spelled `word`, if it is one this tree keeps.
    pub fn from_keyword(word: &str) -> Option<Modifiers> {
        Modifiers::NAMES
            .iter()
            .find(|(_, name)| *name == word)
            .map(|(modifier, _)| *modifier)
    }
}

impl std::fmt::Debug for Modifiers {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = Modifiers::NAMES
            .iter()
            .filter(|(modifier, _)| self.contains(*modifier))
            .map(|(_, name)| *name)
            .collect();
        write!(formatter, "{}", names.join(" "))
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypeParameter {
    pub name: Name,
    pub bounds: Box<[TypeRef]>,
    /// `reified T` of an inline function: its type is known at run time.
    pub reified: bool,
    /// `out T`, `in T` of a class: what a type argument for it may be.
    pub variance: Variance,
}

/// What a declaration says about types, per kind of declaration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signature {
    Class {
        supertypes: Box<[TypeRef]>,
        /// The primary constructor's parameters (Kotlin), or the record
        /// components (Java); `None` without a primary constructor.
        primary_constructor: Option<Box<[Parameter]>>,
    },
    Function {
        receiver: Option<TypeRef>,
        parameters: Box<[Parameter]>,
        /// `None` when not written (`Unit` for a block body, inferred for an
        /// expression body).
        return_type: Option<TypeRef>,
    },
    Property {
        receiver: Option<TypeRef>,
        /// `None` when not written (inferred from the initializer).
        type_ref: Option<TypeRef>,
    },
    TypeAlias {
        target: TypeRef,
    },
    /// Enum entries carry nothing beyond their name.
    None,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Parameter {
    pub name: Name,
    pub type_ref: Option<TypeRef>,
    pub has_default: bool,
    pub vararg: bool,
}

/// A type as written.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypeRef {
    /// `a.b.Outer<X>.Inner<Y>?`
    Named {
        segments: Box<[TypeSegment]>,
        nullable: bool,
    },
    /// `suspend R.(A, B) -> C`
    Function {
        receiver: Option<Box<TypeRef>>,
        parameters: Box<[TypeRef]>,
        result: Box<TypeRef>,
        suspend: bool,
        nullable: bool,
    },
    /// A class as compiled code records it: package and nesting known
    /// exactly, nothing to resolve by scope.
    Qualified {
        package: Path,
        classes: Box<[TypeSegment]>,
        nullable: bool,
    },
    /// A type parameter, as compiled code records it.
    TypeParameter { name: Name, nullable: bool },
    /// A Java primitive or `void`: `int`, `boolean`.
    Primitive { name: Name },
    /// A Java array: `String[]`.
    Array { element: Box<TypeRef> },
    /// Something this tree doesn't read (intersection types, errors).
    Unreadable,
}

impl TypeRef {
    /// The names of a named type's segments: `a`, `b`, `C`.
    pub fn path(&self) -> Option<Path> {
        match self {
            TypeRef::Named { segments, .. } => {
                Some(Path::new(segments.iter().map(|segment| segment.name)))
            }
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeSegment {
    pub name: Name,
    pub arguments: Box<[TypeArgument]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypeArgument {
    Star,
    Type {
        variance: Variance,
        type_ref: TypeRef,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Variance {
    Invariant,
    In,
    Out,
}

/// Where a file's items and imports are in its text. Kept apart from the
/// [`ItemTree`] so that an edit which only moves declarations changes this
/// and not the tree everything else depends on.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ItemSourceMap {
    items: FxHashMap<ItemId, ItemRanges>,
    /// By position in [`ItemTree::imports`]: the imported path's range.
    imports: Box<[TextRange]>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ItemRanges {
    /// The whole declaration.
    pub range: TextRange,
    /// The declared name — where navigation lands.
    pub name_range: TextRange,
}

impl ItemSourceMap {
    /// Where the item is; empty ranges for one not in this file's text.
    pub fn ranges(&self, item: ItemId) -> ItemRanges {
        self.items.get(&item).copied().unwrap_or_default()
    }

    /// The `index`-th import's path.
    pub fn import_range(&self, index: usize) -> TextRange {
        self.imports.get(index).copied().unwrap_or_default()
    }

    /// The items of `tree` whose range contains `offset`, outermost first.
    pub fn items_enclosing(&self, tree: &ItemTree, offset: usize) -> Vec<ItemId> {
        let mut enclosing = Vec::new();
        let mut level: &[ItemId] = &tree.top_level;
        while let Some(found) = level
            .iter()
            .copied()
            .find(|item| self.ranges(*item).range.contains(offset))
        {
            enclosing.push(found);
            level = &tree.item(found).children;
        }
        enclosing
    }

    /// The item whose whole declaration is exactly `range`.
    pub fn item_at(&self, range: TextRange) -> Option<ItemId> {
        self.items
            .iter()
            .find(|(_, ranges)| ranges.range == range)
            .map(|(item, _)| *item)
    }
}

/// Builds an [`ItemTree`] and its [`ItemSourceMap`] item by item, giving
/// each its [`ItemId`].
#[derive(Default)]
pub(crate) struct ItemTreeBuilder {
    items: Vec<Item>,
    ids: Vec<ItemId>,
    positions: FxHashMap<ItemId, u32>,
    /// How many items of a kind and name a parent has had so far.
    ordinals: FxHashMap<(Option<ItemId>, IdentityKind, Name), u32>,
    taken: FxHashSet<ItemId>,
    ranges: FxHashMap<ItemId, ItemRanges>,
    deprecations: FxHashMap<ItemId, Deprecation>,
    annotations: FxHashMap<ItemId, Box<[Path]>>,
    opt_in_markers: FxHashMap<ItemId, OptInMarker>,
    contracts: FxHashMap<ItemId, Contract>,
}

impl ItemTreeBuilder {
    /// Records the pushed item `id`'s annotation classes.
    pub(crate) fn annotate(&mut self, id: ItemId, annotations: Box<[Path]>) {
        if !annotations.is_empty() {
            self.annotations.insert(id, annotations);
        }
    }

    /// Gives the pushed function `id` its contract.
    pub(crate) fn add_contract(&mut self, id: ItemId, contract: Contract) {
        self.contracts.insert(id, contract);
    }

    /// Marks the pushed annotation class `id` as an opt-in marker.
    pub(crate) fn mark_opt_in(&mut self, id: ItemId, marker: OptInMarker) {
        self.opt_in_markers.insert(id, marker);
    }

    /// Marks the pushed item `id` deprecated, as `deprecation` says.
    pub(crate) fn deprecate(&mut self, id: ItemId, deprecation: Deprecation) {
        if let Some(position) = self.positions.get(&id) {
            self.items[*position as usize]
                .modifiers
                .insert(Modifiers::DEPRECATED);
        }
        self.deprecations.insert(id, deprecation);
    }

    /// Adds `item` (its `parent` set, its children to come) found at
    /// `ranges`.
    pub(crate) fn push(&mut self, item: Item, ranges: ItemRanges) -> ItemId {
        let id = self.allocate(item.parent, IdentityKind::of(&item.kind), item.name);
        self.positions.insert(id, self.items.len() as u32);
        self.items.push(item);
        self.ids.push(id);
        if ranges != ItemRanges::default() {
            self.ranges.insert(id, ranges);
        }
        id
    }

    fn allocate(&mut self, parent: Option<ItemId>, kind: IdentityKind, name: Name) -> ItemId {
        let ordinal = self.ordinals.entry((parent, kind, name)).or_default();
        let mut hasher = rustc_hash::FxHasher::default();
        (parent, kind, name.as_str(), *ordinal).hash(&mut hasher);
        *ordinal += 1;
        let mut candidate = ItemId(hasher.finish() as u32);
        // A collision (rare) takes the next free value, in document order.
        while !self.taken.insert(candidate) {
            candidate = ItemId(candidate.0.wrapping_add(1));
        }
        candidate
    }

    pub(crate) fn item(&self, id: ItemId) -> &Item {
        &self.items[self.positions[&id] as usize]
    }

    pub(crate) fn set_children(&mut self, parent: ItemId, children: Vec<ItemId>) {
        let position = self.positions[&parent] as usize;
        self.items[position].children = children.into_boxed_slice();
    }

    pub(crate) fn finish(
        self,
        package: Path,
        imports: Vec<(Import, TextRange)>,
        top_level: Vec<ItemId>,
        source_files: Vec<(ItemId, Box<str>)>,
    ) -> (ItemTree, ItemSourceMap) {
        let (imports, import_ranges): (Vec<Import>, Vec<TextRange>) = imports.into_iter().unzip();
        let mut named: FxHashMap<(Option<ItemId>, Name), Vec<ItemId>> = FxHashMap::default();
        for (id, item) in self.ids.iter().zip(self.items.iter()) {
            named.entry((item.parent, item.name)).or_default().push(*id);
        }
        let tree = ItemTree {
            package,
            source_files: source_files.into_boxed_slice(),
            imports: imports.into_boxed_slice(),
            items: self.items.into_boxed_slice(),
            ids: self.ids.into_boxed_slice(),
            positions: self.positions,
            named: named
                .into_iter()
                .map(|(key, items)| (key, items.into_boxed_slice()))
                .collect(),
            top_level: top_level.into_boxed_slice(),
            deprecations: self.deprecations,
            annotations: self.annotations,
            opt_in_markers: self.opt_in_markers,
            contracts: self.contracts,
        };
        let source_map = ItemSourceMap {
            items: self.ranges,
            imports: import_ranges.into_boxed_slice(),
        };
        (tree, source_map)
    }
}
