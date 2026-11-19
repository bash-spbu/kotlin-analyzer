//! Properties as the data flow sees them. The compiler smart-casts a stable
//! property read from a stable value as it does a local (`order.status`
//! after `order.status != null`), and never one that isn't stable: a `var`,
//! a property with a getter or a delegate, `lateinit`, an extension, an
//! `expect` one, or a public one of another module (which may be recompiled
//! apart). A stable property read gets a path, which smart casts narrow.

use base_db::SourceFile;
use hir_def::item_tree::{ClassKind, ItemKind, Modifiers, Signature, Visibility};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use syntax::{SyntaxNode, named_children};

use super::declarations::Inferrer;
use super::narrowing::{Narrowed, PropertyPathId};
use super::{LocalId, LocalKind, Reference};
use crate::ty::Ty;

/// A stable property read from a stable value: its type as read.
pub(super) struct PropertyPath {
    pub(super) ty: Ty,
    pub(super) root: PathRoot,
}

/// The stable value a property path reads from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum PathRoot {
    Local(LocalId),
    /// `this` of the implicit receiver at this depth of the receiver stack.
    Receiver(u32),
    /// No value: a top-level property or an object's.
    Static,
    Property(PropertyPathId),
}

/// What reading a property is to the data flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PropertyRead {
    /// Smart-cast as a local.
    Stable(PropertyPathId),
    /// Never smart-cast: its type is the declared one.
    Unstable,
    /// A library's: never smart-cast, but the member found may not be the
    /// one the compiler reads (a Java exception's `getMessage()`
    /// overriding `Throwable.message`).
    Library,
    /// The compiler may smart-cast it where this inference can't tell (an
    /// `open` property, one read from a `var`).
    Unsure,
}

/// What a property's receiver is to the data flow.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ReceiverRoot {
    Stable(PathRoot),
    /// Not a value the compiler tracks (a call's result): its properties
    /// are never smart-cast.
    Untracked,
    Unsure,
}

/// How the compiler treats a property declaration, receiver aside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stability {
    Stable,
    Unstable,
    Library,
    Unsure,
}

impl Inferrer<'_> {
    /// Records how the data flow sees the property `location` named by
    /// `name` and read from `root`, and gives its type there: as narrowed
    /// when it's stable, else `ty` as read.
    pub(super) fn read_property(
        &mut self,
        name: SyntaxNode<'_>,
        root: ReceiverRoot,
        location: ItemLocation,
        ty: Ty,
    ) -> Ty {
        let read = match (self.property_stability(location), root) {
            (Stability::Library, _) => PropertyRead::Library,
            (Stability::Unstable, _) | (_, ReceiverRoot::Untracked) => PropertyRead::Unstable,
            (Stability::Unsure, _) | (_, ReceiverRoot::Unsure) => PropertyRead::Unsure,
            (Stability::Stable, ReceiverRoot::Stable(root)) => {
                PropertyRead::Stable(self.property_path(root, location, &ty))
            }
        };
        self.property_reads.insert(name.start_byte() as u32, read);
        match read {
            PropertyRead::Stable(path) => self.narrowed_type(Narrowed::Property(path)),
            PropertyRead::Unstable | PropertyRead::Library | PropertyRead::Unsure => ty,
        }
    }

    /// A property's type at `name` before smart casts: its path's.
    pub(super) fn declared_property_type(&self, name: SyntaxNode<'_>) -> Option<Ty> {
        match self.property_read_at(name)? {
            PropertyRead::Stable(path) => Some(self.property_paths[path.0 as usize].ty.clone()),
            PropertyRead::Unstable | PropertyRead::Library | PropertyRead::Unsure => None,
        }
    }

    /// Whether `subject` is read from an implicit receiver at `depth` or
    /// deeper: one a lambda brought into scope, gone once it ended.
    pub(super) fn reads_receiver_from(&self, subject: Narrowed, depth: usize) -> bool {
        let mut current = subject;
        loop {
            let path = match current {
                Narrowed::Property(path) => path,
                Narrowed::Receiver(at) => return at as usize >= depth,
                Narrowed::Local(_) => return false,
            };
            match self.property_paths[path.0 as usize].root {
                PathRoot::Receiver(at) => return at as usize >= depth,
                PathRoot::Property(outer) => current = Narrowed::Property(outer),
                PathRoot::Local(_) | PathRoot::Static => return false,
            }
        }
    }

    /// How the data flow saw the property read named at `name`, if one was.
    pub(super) fn property_read_at(&self, name: SyntaxNode<'_>) -> Option<PropertyRead> {
        self.property_reads
            .get(&(name.start_byte() as u32))
            .copied()
    }

    /// What a smart cast of `node` narrows: a local, or a stable property
    /// (`order.status`, `order?.status`, `status` of `this`).
    pub(super) fn narrowable(&self, node: SyntaxNode<'_>) -> Option<Narrowed> {
        match node.kind() {
            "parenthesized_expression" => self.narrowable(named_children(node).next()?),
            "simple_identifier" => match self.lookup_value_local(self.name(node)) {
                Some(local) => Some(Narrowed::Local(local)),
                None => self.stable_read(node),
            },
            "navigation_expression" => self.stable_read(property_name(node)?),
            "this_expression" => Some(Narrowed::Receiver(self.this_receiver(node)? as u32)),
            _ => None,
        }
    }

    fn stable_read(&self, name: SyntaxNode<'_>) -> Option<Narrowed> {
        match self.property_read_at(name)? {
            PropertyRead::Stable(path) => Some(Narrowed::Property(path)),
            PropertyRead::Unstable | PropertyRead::Library | PropertyRead::Unsure => None,
        }
    }

    /// What `receiver` (of `receiver.property`) is to the data flow.
    pub(super) fn receiver_root(&self, receiver: SyntaxNode<'_>) -> ReceiverRoot {
        match receiver.kind() {
            "parenthesized_expression" => match named_children(receiver).next() {
                Some(inner) => self.receiver_root(inner),
                None => ReceiverRoot::Unsure,
            },
            "this_expression" => self.this_root(receiver),
            "simple_identifier" => match self.recorded_at(receiver).map(|name| name.reference) {
                Some(Reference::Local(local)) => self.local_root(local),
                Some(Reference::Declaration(location)) if self.is_object(location) => {
                    ReceiverRoot::Stable(PathRoot::Static)
                }
                _ => self.read_root(receiver),
            },
            "navigation_expression" => match property_name(receiver) {
                Some(name) => self.read_root(name),
                None => ReceiverRoot::Unsure,
            },
            "call_expression" | "string_literal" | "integer_literal" => ReceiverRoot::Untracked,
            _ => ReceiverRoot::Unsure,
        }
    }

    /// The root of a property read without a receiver written: the
    /// implicit receiver at `depth`.
    pub(super) fn implicit_root(depth: usize) -> ReceiverRoot {
        ReceiverRoot::Stable(PathRoot::Receiver(depth as u32))
    }

    fn this_root(&self, this: SyntaxNode<'_>) -> ReceiverRoot {
        // `this@Outer`: which receiver the label names isn't followed.
        let labelled = named_children(this).next().is_some();
        if labelled || self.receivers.is_empty() {
            return ReceiverRoot::Unsure;
        }
        Self::implicit_root(self.receivers.len() - 1)
    }

    fn local_root(&self, local: LocalId) -> ReceiverRoot {
        if self.is_stable(Narrowed::Local(local)) {
            return ReceiverRoot::Stable(PathRoot::Local(local));
        }
        match self.local_kind(local) {
            LocalKind::Function | LocalKind::Class => ReceiverRoot::Untracked,
            _ => ReceiverRoot::Unsure,
        }
    }

    fn read_root(&self, name: SyntaxNode<'_>) -> ReceiverRoot {
        match self.property_read_at(name) {
            Some(PropertyRead::Stable(path)) => ReceiverRoot::Stable(PathRoot::Property(path)),
            Some(PropertyRead::Unstable | PropertyRead::Library) => ReceiverRoot::Untracked,
            Some(PropertyRead::Unsure) | None => ReceiverRoot::Unsure,
        }
    }

    fn is_object(&self, location: ItemLocation) -> bool {
        let tree = item_tree(self.db, location.container);
        matches!(
            tree.item(location.item).kind,
            ItemKind::Class(ClassKind::Object | ClassKind::CompanionObject)
        )
    }

    fn property_path(&mut self, root: PathRoot, location: ItemLocation, ty: &Ty) -> PropertyPathId {
        if let Some(path) = self.path_ids.get(&(root, location)) {
            return *path;
        }
        let path = PropertyPathId(self.property_paths.len() as u32);
        self.property_paths.push(PropertyPath {
            ty: ty.clone(),
            root,
        });
        self.path_ids.insert((root, location), path);
        path
    }

    /// The compiler's rules for a property's stability (its
    /// `PropertyStability`), receiver aside.
    fn property_stability(&self, location: ItemLocation) -> Stability {
        let tree = item_tree(self.db, location.container);
        let item = tree.item(location.item);
        let ItemKind::Property { mutable } = item.kind else {
            return Stability::Unsure;
        };
        let has_extension_receiver = matches!(
            item.signature,
            Signature::Property {
                receiver: Some(_),
                ..
            }
        );
        let unstable = mutable
            || item.modifiers.contains(Modifiers::LATEINIT)
            || item.modifiers.contains(Modifiers::GETTER)
            || item.modifiers.contains(Modifiers::DELEGATED)
            || item.modifiers.contains(Modifiers::EXPECT)
            || has_extension_receiver;
        if unstable {
            return Stability::Unstable;
        }
        let ItemContainer::File(file) = location.container else {
            // A library is another module.
            return Stability::Library;
        };
        if file.language(self.db) != syntax::Language::Kotlin {
            return Stability::Unsure;
        }
        if item.visibility == Visibility::Private {
            return Stability::Stable;
        }
        match self.same_module(file) {
            Some(false) => return Stability::Unstable,
            None => return Stability::Unsure,
            Some(true) => {}
        }
        if self.may_be_overridden(location) {
            Stability::Unsure
        } else {
            Stability::Stable
        }
    }

    /// Whether `file` is in this file's Gradle module (whose source sets
    /// are friends: `main` and `test`), when both are known.
    fn same_module(&self, file: SourceFile) -> Option<bool> {
        let model = self.db.workspace().model(self.db);
        let own = model.source_set(self.file.source_set(self.db)?).module;
        let other = model.source_set(file.source_set(self.db)?).module;
        Some(own == other)
    }

    /// Whether a property is `open`: written so, `abstract`, or an
    /// `override` not `final` in a class that can be extended.
    fn may_be_overridden(&self, location: ItemLocation) -> bool {
        let tree = item_tree(self.db, location.container);
        let item = tree.item(location.item);
        let modifiers = item.modifiers;
        if modifiers.contains(Modifiers::OPEN) || modifiers.contains(Modifiers::ABSTRACT) {
            return true;
        }
        let Some(parent) = item.parent else {
            return false;
        };
        let class = tree.item(parent);
        let class_extendable = matches!(class.kind, ItemKind::Class(ClassKind::Interface))
            || class.modifiers.contains(Modifiers::OPEN)
            || class.modifiers.contains(Modifiers::ABSTRACT)
            || class.modifiers.contains(Modifiers::SEALED);
        let inherited_openness =
            modifiers.contains(Modifiers::OVERRIDE) && !modifiers.contains(Modifiers::FINAL);
        let in_interface = matches!(class.kind, ItemKind::Class(ClassKind::Interface));
        in_interface || inherited_openness && class_extendable
    }
}

/// The property name of `receiver.name` or `receiver?.name`.
pub(super) fn property_name(navigation: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let suffix = named_children(navigation).last()?;
    named_children(suffix).find(|part| part.kind() == "simple_identifier")
}
