//! One file's declarations and their signatures, without bodies.
//!
//! An `ItemTree` is what the rest of a module needs to know about a file:
//! its package, imports, and every declaration with the types it names.
//! Function bodies and initializers aren't in it, so editing inside a body
//! changes nothing that only depends on declarations.

mod lower_java;
mod lower_kotlin;
mod pretty;

use intern::{Name, Path};
use syntax::TextRange;

pub(crate) use lower_java::lower_java;
pub(crate) use lower_kotlin::lower_kotlin;

/// An item's position in its file's [`ItemTree`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ItemIndex(pub u32);

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ItemTree {
    /// The file's package; the root package when it declares none.
    pub package: Path,
    pub imports: Box<[Import]>,
    /// Every declaration of the file, nested ones included, in document
    /// order; `top_level` and each item's `children` index into it.
    pub items: Box<[Item]>,
    pub top_level: Box<[ItemIndex]>,
}

impl ItemTree {
    pub fn item(&self, index: ItemIndex) -> &Item {
        &self.items[index.0 as usize]
    }

    /// Every item with its index.
    pub fn indexed_items(&self) -> impl Iterator<Item = (ItemIndex, &Item)> {
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| (ItemIndex(index as u32), item))
    }

    /// The items whose range contains `offset`, outermost first.
    pub fn items_enclosing(&self, offset: usize) -> Vec<ItemIndex> {
        let mut enclosing = Vec::new();
        let mut level: &[ItemIndex] = &self.top_level;
        while let Some(found) = level
            .iter()
            .copied()
            .find(|index| self.item(*index).range.contains(offset))
        {
            enclosing.push(found);
            level = &self.item(found).children;
        }
        enclosing
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
    /// The range of the imported path.
    pub range: TextRange,
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
    /// The declared name's range — where navigation lands.
    pub name_range: TextRange,
    /// The whole declaration's range.
    pub range: TextRange,
    pub parent: Option<ItemIndex>,
    pub children: Box<[ItemIndex]>,
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

    const NAMES: [(Modifiers, &'static str); 19] = [
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
        range: TextRange,
    },
    /// `suspend R.(A, B) -> C`
    Function {
        receiver: Option<Box<TypeRef>>,
        parameters: Box<[TypeRef]>,
        result: Box<TypeRef>,
        suspend: bool,
        nullable: bool,
    },
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
