//! Kotlin syntax → [`ItemTree`].

use intern::{Name, Path};
use syntax::ast::AstNode;
use syntax::ast::kotlin::{
    ClassDeclaration, ClassKeyword, CompanionObject, Declaration, FunctionDeclaration,
    FunctionType, FunctionValueParameters, Modifiers as ModifiersNode, ObjectDeclaration,
    ParameterBinding, PropertyDeclaration, SourceFile, TypeParameters, UserType,
    Variance as SyntaxVariance,
};
use syntax::{Parse, SyntaxNode, TextRange, has_token, named_children};

use super::{
    ClassKind, Import, ImportTarget, Item, ItemId, ItemKind, ItemRanges, ItemSourceMap, ItemTree,
    ItemTreeBuilder, Modifiers, Parameter, Signature, TypeArgument, TypeParameter, TypeRef,
    TypeSegment, Variance, Visibility,
};

pub(crate) fn lower_kotlin(parse: &Parse) -> (ItemTree, ItemSourceMap) {
    let Some(file) = SourceFile::cast(parse.root()) else {
        return (ItemTree::default(), ItemSourceMap::default());
    };
    let mut lowering = Lowering {
        parse,
        builder: ItemTreeBuilder::default(),
    };
    let package = file
        .package_header()
        .and_then(|header| header.path())
        .map(|path| lowering.dotted(path.segments()))
        .unwrap_or_default();
    let imports = file
        .imports()
        .filter_map(|import| lowering.import(import))
        .collect();
    let top_level = file
        .declarations()
        .filter_map(|declaration| lowering.declaration(declaration, None, Container::File))
        .collect();
    lowering
        .builder
        .finish(package, imports, top_level, Vec::new())
}

/// A type written in a Kotlin file (a type node: `user_type`,
/// `nullable_type`, `function_type`, …), as a [`TypeRef`] — for the types
/// bodies write, which the item tree doesn't hold.
pub fn kotlin_type_ref(parse: &Parse, node: SyntaxNode<'_>) -> TypeRef {
    let lowering = Lowering {
        parse,
        builder: ItemTreeBuilder::default(),
    };
    lowering.type_ref(node)
}

/// What a declaration is directly inside — which decides its defaults.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    File,
    Class,
    Interface,
}

struct Lowering<'parse> {
    parse: &'parse Parse,
    builder: ItemTreeBuilder,
}

/// An item about to be added, before its children are known.
struct NewItem {
    name: Name,
    kind: ItemKind,
    name_range: TextRange,
    range: TextRange,
    visibility: Visibility,
    modifiers: Modifiers,
    type_parameters: Box<[TypeParameter]>,
    signature: Signature,
}

impl<'parse> Lowering<'parse> {
    fn text(&self, node: SyntaxNode<'_>) -> &'parse str {
        let range = node.byte_range();
        &self.parse.text()[range]
    }

    fn name(&self, node: SyntaxNode<'_>) -> Name {
        Name::new(self.text(node).trim_matches('`'))
    }

    fn dotted<'tree>(&self, segments: impl Iterator<Item = SyntaxNode<'tree>>) -> Path {
        Path::new(segments.map(|segment| self.name(segment)))
    }

    /// An import and its path's range.
    fn import(&self, import: syntax::ast::kotlin::ImportHeader<'_>) -> Option<(Import, TextRange)> {
        let path_node = import.path()?;
        let target = if import.is_wildcard() {
            ImportTarget::Everything
        } else {
            ImportTarget::Single {
                alias: import.alias().map(|alias| self.name(alias)),
            }
        };
        let import = Import {
            path: self.dotted(path_node.segments()),
            target,
        };
        Some((import, path_node.range()))
    }

    fn push(&mut self, item: NewItem, parent: Option<ItemId>) -> ItemId {
        let ranges = ItemRanges {
            range: item.range,
            name_range: item.name_range,
        };
        let new = Item {
            name: item.name,
            kind: item.kind,
            parent,
            children: Box::new([]),
            visibility: item.visibility,
            modifiers: item.modifiers,
            type_parameters: item.type_parameters,
            signature: item.signature,
        };
        let deprecated = new.modifiers.contains(Modifiers::DEPRECATED);
        let is_annotation_class = new.kind == ItemKind::Class(ClassKind::Annotation);
        let is_function = new.kind == ItemKind::Function;
        let id = self.builder.push(new, ranges);
        let Some(declaration) = self.node_spanning(item.range) else {
            return id;
        };
        if deprecated
            && let Some(deprecation) =
                super::written_annotations::written_deprecation(self.parse, declaration)
        {
            self.builder.deprecate(id, deprecation);
        }
        let annotations =
            super::written_annotations::written_annotation_classes(self.parse, declaration);
        self.builder.annotate(id, annotations);
        if is_annotation_class
            && let Some(marker) =
                super::written_annotations::written_opt_in_marker(self.parse, declaration)
        {
            self.builder.mark_opt_in(id, marker);
        }
        if is_function
            && let Some(contract) =
                super::written_contract::written_contract(self.parse, declaration)
        {
            self.builder.add_contract(id, contract);
        }
        id
    }

    /// The node that spans exactly `range`: an item's declaration.
    fn node_spanning(&self, range: TextRange) -> Option<SyntaxNode<'parse>> {
        let mut node = self
            .parse
            .root()
            .descendant_for_byte_range(range.start as usize, range.end as usize)?;
        while TextRange::of(node) != range {
            node = node.parent()?;
        }
        Some(node)
    }

    fn set_children(&mut self, parent: ItemId, children: Vec<ItemId>) {
        self.builder.set_children(parent, children);
    }

    fn declaration(
        &mut self,
        declaration: Declaration<'_>,
        parent: Option<ItemId>,
        container: Container,
    ) -> Option<ItemId> {
        match declaration {
            Declaration::Class(class) => self.class(class, parent),
            Declaration::Object(object) => self.object(object, parent),
            Declaration::Companion(companion) => self.companion(companion, parent),
            Declaration::Function(function) => self.function(function, parent, container),
            Declaration::Property(property) => self.property(property, parent, container),
            Declaration::TypeAlias(alias) => {
                let name_node = alias.name()?;
                let (visibility, modifiers) = self.modifiers(alias.modifiers(), container);
                let target = alias
                    .target()
                    .map(|target| self.type_ref(target))
                    .unwrap_or(TypeRef::Unreadable);
                let item = NewItem {
                    name: self.name(name_node),
                    kind: ItemKind::TypeAlias,
                    name_range: TextRange::of(name_node),
                    range: TextRange::of(alias.syntax()),
                    visibility,
                    modifiers,
                    type_parameters: self.type_parameters(alias.type_parameters(), alias.syntax()),
                    signature: Signature::TypeAlias { target },
                };
                Some(self.push(item, parent))
            }
            Declaration::SecondaryConstructor(constructor) => {
                let parent_index = parent?;
                let (visibility, modifiers) = self.modifiers(constructor.modifiers(), container);
                let keyword = keyword_range(constructor.syntax(), "constructor")
                    .unwrap_or_else(|| TextRange::of(constructor.syntax()));
                let item = NewItem {
                    name: self.builder.item(parent_index).name,
                    kind: ItemKind::Constructor,
                    name_range: keyword,
                    range: TextRange::of(constructor.syntax()),
                    visibility,
                    modifiers,
                    type_parameters: Box::new([]),
                    signature: Signature::Function {
                        receiver: None,
                        parameters: self.parameters(constructor.parameters()),
                        return_type: None,
                    },
                };
                Some(self.push(item, parent))
            }
            Declaration::EnumEntry(entry) => {
                let name_node = entry.name()?;
                let item = NewItem {
                    name: self.name(name_node),
                    kind: ItemKind::EnumEntry,
                    name_range: TextRange::of(name_node),
                    range: TextRange::of(entry.syntax()),
                    visibility: Visibility::Public,
                    modifiers: Modifiers::default(),
                    type_parameters: Box::new([]),
                    signature: Signature::None,
                };
                Some(self.push(item, parent))
            }
        }
    }

    fn class(&mut self, class: ClassDeclaration<'_>, parent: Option<ItemId>) -> Option<ItemId> {
        let name_node = class.name()?;
        let container = if parent.is_some() {
            Container::Class
        } else {
            Container::File
        };
        let (visibility, mut modifiers) = self.modifiers(class.modifiers(), container);
        let annotation = class.modifiers().is_some_and(|modifiers| {
            modifiers
                .words()
                .any(|word| word.kind() == "class_modifier" && has_token(word, "annotation"))
        });
        let kind = match class.keyword() {
            ClassKeyword::Interface => ClassKind::Interface,
            ClassKeyword::FunInterface => {
                modifiers.insert(Modifiers::FUN_INTERFACE);
                ClassKind::Interface
            }
            ClassKeyword::Enum => ClassKind::Enum,
            ClassKeyword::Class if annotation => ClassKind::Annotation,
            ClassKeyword::Class => ClassKind::Class,
        };
        let primary_constructor = class.primary_constructor().map(|constructor| {
            constructor
                .parameters()
                .filter_map(|parameter| {
                    Some(Parameter {
                        name: self.name(parameter.name()?),
                        type_ref: parameter.type_node().map(|node| self.type_ref(node)),
                        has_default: parameter.has_default(),
                        vararg: parameter.is_vararg(),
                    })
                })
                .collect::<Box<[Parameter]>>()
        });
        let item = NewItem {
            name: self.name(name_node),
            kind: ItemKind::Class(kind),
            name_range: TextRange::of(name_node),
            range: TextRange::of(class.syntax()),
            visibility,
            modifiers,
            type_parameters: self.type_parameters(class.type_parameters(), class.syntax()),
            signature: Signature::Class {
                supertypes: self.supertypes(class.supertypes()),
                primary_constructor,
            },
        };
        let index = self.push(item, parent);
        let mut children = Vec::new();
        // `val`/`var` constructor parameters are properties of the class.
        if let Some(constructor) = class.primary_constructor() {
            for parameter in constructor.parameters() {
                let mutable = match parameter.binding() {
                    ParameterBinding::None => continue,
                    ParameterBinding::Val => false,
                    ParameterBinding::Var => true,
                };
                let Some(name_node) = parameter.name() else {
                    continue;
                };
                let (visibility, modifiers) =
                    self.modifiers(parameter.modifiers(), Container::Class);
                let property = NewItem {
                    name: self.name(name_node),
                    kind: ItemKind::Property { mutable },
                    name_range: TextRange::of(name_node),
                    range: TextRange::of(parameter.syntax()),
                    visibility,
                    modifiers,
                    type_parameters: Box::new([]),
                    signature: Signature::Property {
                        receiver: None,
                        type_ref: parameter.type_node().map(|node| self.type_ref(node)),
                    },
                };
                children.push(self.push(property, Some(index)));
            }
        }
        let member_container = match kind {
            ClassKind::Interface => Container::Interface,
            _ => Container::Class,
        };
        for member in class.members() {
            if let Some(child) = self.declaration(member, Some(index), member_container) {
                children.push(child);
            }
        }
        self.set_children(index, children);
        Some(index)
    }

    fn object(&mut self, object: ObjectDeclaration<'_>, parent: Option<ItemId>) -> Option<ItemId> {
        let name_node = object.name()?;
        let container = if parent.is_some() {
            Container::Class
        } else {
            Container::File
        };
        let (visibility, modifiers) = self.modifiers(object.modifiers(), container);
        let item = NewItem {
            name: self.name(name_node),
            kind: ItemKind::Class(ClassKind::Object),
            name_range: TextRange::of(name_node),
            range: TextRange::of(object.syntax()),
            visibility,
            modifiers,
            type_parameters: Box::new([]),
            signature: Signature::Class {
                supertypes: self.supertypes(object.supertypes()),
                primary_constructor: None,
            },
        };
        let index = self.push(item, parent);
        let children = object
            .members()
            .filter_map(|member| self.declaration(member, Some(index), Container::Class))
            .collect();
        self.set_children(index, children);
        Some(index)
    }

    fn companion(
        &mut self,
        companion: CompanionObject<'_>,
        parent: Option<ItemId>,
    ) -> Option<ItemId> {
        let (visibility, modifiers) = self.modifiers(companion.modifiers(), Container::Class);
        let (name, name_range) = match companion.name() {
            Some(name_node) => (self.name(name_node), TextRange::of(name_node)),
            None => (
                Name::new("Companion"),
                keyword_range(companion.syntax(), "object")
                    .unwrap_or_else(|| TextRange::of(companion.syntax())),
            ),
        };
        let item = NewItem {
            name,
            kind: ItemKind::Class(ClassKind::CompanionObject),
            name_range,
            range: TextRange::of(companion.syntax()),
            visibility,
            modifiers,
            type_parameters: Box::new([]),
            signature: Signature::Class {
                supertypes: self.supertypes(companion.supertypes()),
                primary_constructor: None,
            },
        };
        let index = self.push(item, parent);
        let children = companion
            .members()
            .filter_map(|member| self.declaration(member, Some(index), Container::Class))
            .collect();
        self.set_children(index, children);
        Some(index)
    }

    fn function(
        &mut self,
        function: FunctionDeclaration<'_>,
        parent: Option<ItemId>,
        container: Container,
    ) -> Option<ItemId> {
        let name_node = function.name()?;
        let (visibility, mut modifiers) = self.modifiers(function.modifiers(), container);
        let abstract_in_interface = container == Container::Interface && !function.has_body();
        if abstract_in_interface {
            modifiers.insert(Modifiers::ABSTRACT);
        }
        let item = NewItem {
            name: self.name(name_node),
            kind: ItemKind::Function,
            name_range: TextRange::of(name_node),
            range: TextRange::of(function.syntax()),
            visibility,
            modifiers,
            type_parameters: self.type_parameters(function.type_parameters(), function.syntax()),
            signature: Signature::Function {
                receiver: function.receiver().map(|node| self.type_ref(node)),
                parameters: self.parameters(function.parameters()),
                return_type: function.return_type().map(|node| self.type_ref(node)),
            },
        };
        Some(self.push(item, parent))
    }

    fn property(
        &mut self,
        property: PropertyDeclaration<'_>,
        parent: Option<ItemId>,
        container: Container,
    ) -> Option<ItemId> {
        let variable = property.variable()?;
        let name_node = variable.name()?;
        let (visibility, mut modifiers) = self.modifiers(property.modifiers(), container);
        if property.has_getter() {
            modifiers.insert(Modifiers::GETTER);
        }
        if property.is_delegated() {
            modifiers.insert(Modifiers::DELEGATED);
        }
        let item = NewItem {
            name: self.name(name_node),
            kind: ItemKind::Property {
                mutable: property.is_mutable(),
            },
            name_range: TextRange::of(name_node),
            range: TextRange::of(property.syntax()),
            visibility,
            modifiers,
            type_parameters: self.type_parameters(property.type_parameters(), property.syntax()),
            signature: Signature::Property {
                receiver: property.receiver().map(|node| self.type_ref(node)),
                type_ref: variable.type_node().map(|node| self.type_ref(node)),
            },
        };
        Some(self.push(item, parent))
    }

    fn parameters(&self, parameters: Option<FunctionValueParameters<'_>>) -> Box<[Parameter]> {
        let Some(parameters) = parameters else {
            return Box::new([]);
        };
        parameters
            .entries()
            .into_iter()
            .filter_map(|entry| {
                Some(Parameter {
                    name: self.name(entry.parameter.name()?),
                    type_ref: entry.parameter.type_node().map(|node| self.type_ref(node)),
                    has_default: entry.has_default,
                    vararg: entry.vararg,
                })
            })
            .collect()
    }

    /// The type parameters with their bounds: the one written beside each
    /// (`<B : Scheme>`) and those of the declaration's `where` clause
    /// (`where B : Scheme, B : Named`).
    fn type_parameters(
        &self,
        parameters: Option<TypeParameters<'_>>,
        declaration: SyntaxNode<'_>,
    ) -> Box<[TypeParameter]> {
        let Some(parameters) = parameters else {
            return Box::new([]);
        };
        let constraints = self.where_constraints(declaration);
        parameters
            .parameters()
            .filter_map(|parameter| {
                let name = self.name(parameter.name()?);
                let written_beside = parameter.bound().map(|bound| self.type_ref(bound));
                let from_where = constraints
                    .iter()
                    .filter(|(constrained, _)| *constrained == name)
                    .map(|(_, bound)| bound.clone());
                Some(TypeParameter {
                    name,
                    bounds: written_beside.into_iter().chain(from_where).collect(),
                    reified: parameter.is_reified(),
                })
            })
            .collect()
    }

    /// `where T : Bound` pairs of a declaration.
    fn where_constraints(&self, declaration: SyntaxNode<'_>) -> Vec<(Name, TypeRef)> {
        let Some(clause) =
            syntax::named_children(declaration).find(|child| child.kind() == "type_constraints")
        else {
            return Vec::new();
        };
        syntax::named_children(clause)
            .filter(|child| child.kind() == "type_constraint")
            .filter_map(|constraint| {
                let mut parts = syntax::named_children(constraint)
                    .filter(|part| !part.kind().contains("annotation"));
                let constrained = parts
                    .next()
                    .filter(|part| part.kind() == "type_identifier")?;
                let bound = parts.next()?;
                Some((self.name(constrained), self.type_ref(bound)))
            })
            .collect()
    }

    fn supertypes<'tree>(
        &self,
        specifiers: impl Iterator<Item = syntax::ast::kotlin::DelegationSpecifier<'tree>>,
    ) -> Box<[TypeRef]> {
        specifiers
            .filter_map(|specifier| specifier.type_node())
            .map(|node| self.type_ref(node))
            .collect()
    }

    /// Visibility (with the container's default) and the kept modifiers.
    fn modifiers(
        &self,
        modifiers: Option<ModifiersNode<'_>>,
        container: Container,
    ) -> (Visibility, Modifiers) {
        let mut visibility = Visibility::Public;
        let mut kept = Modifiers::default();
        let _ = container;
        let Some(modifiers) = modifiers else {
            return (visibility, kept);
        };
        for word in modifiers.words() {
            let text = self.text(word).trim();
            match word.kind() {
                "visibility_modifier" => {
                    visibility = match text {
                        "private" => Visibility::Private,
                        "protected" => Visibility::Protected,
                        "internal" => Visibility::Internal,
                        _ => Visibility::Public,
                    };
                }
                "annotation" => {
                    let deprecated = text.trim_start_matches('@').starts_with("Deprecated")
                        || text.contains(".Deprecated");
                    if deprecated {
                        kept.insert(Modifiers::DEPRECATED);
                    }
                }
                _ => {
                    if let Some(modifier) = Modifiers::from_keyword(text) {
                        kept.insert(modifier);
                    }
                }
            }
        }
        (visibility, kept)
    }

    fn type_ref(&self, node: SyntaxNode<'_>) -> TypeRef {
        match node.kind() {
            "user_type" => {
                let user_type = UserType::new_unchecked(node);
                let segments = user_type
                    .segments()
                    .into_iter()
                    .map(|(name, arguments)| TypeSegment {
                        name: self.name(name),
                        arguments: arguments
                            .map(|arguments| {
                                arguments
                                    .projections()
                                    .map(|projection| match projection.type_node() {
                                        None => TypeArgument::Star,
                                        Some(type_node) => TypeArgument::Type {
                                            variance: match projection.variance() {
                                                SyntaxVariance::Invariant => Variance::Invariant,
                                                SyntaxVariance::In => Variance::In,
                                                SyntaxVariance::Out => Variance::Out,
                                            },
                                            type_ref: self.type_ref(type_node),
                                        },
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect();
                TypeRef::Named {
                    segments,
                    nullable: false,
                }
            }
            "nullable_type" => {
                let inner = named_children(node).find(|child| syntax::ast::kotlin::is_type(*child));
                match inner.map(|inner| self.type_ref(inner)) {
                    Some(TypeRef::Named { segments, .. }) => TypeRef::Named {
                        segments,
                        nullable: true,
                    },
                    Some(TypeRef::Function {
                        receiver,
                        parameters,
                        result,
                        suspend,
                        ..
                    }) => TypeRef::Function {
                        receiver,
                        parameters,
                        result,
                        suspend,
                        nullable: true,
                    },
                    Some(other) => other,
                    None => TypeRef::Unreadable,
                }
            }
            "parenthesized_type" | "not_nullable_type" => named_children(node)
                .find(|child| syntax::ast::kotlin::is_type(*child))
                .map(|inner| self.type_ref(inner))
                .unwrap_or(TypeRef::Unreadable),
            "function_type" => {
                let function = FunctionType::new_unchecked(node);
                TypeRef::Function {
                    receiver: function
                        .receiver()
                        .map(|receiver| Box::new(self.type_ref(receiver))),
                    parameters: function
                        .parameters()
                        .into_iter()
                        .map(|parameter| self.type_ref(parameter))
                        .collect(),
                    result: Box::new(
                        function
                            .result()
                            .map(|result| self.type_ref(result))
                            .unwrap_or(TypeRef::Unreadable),
                    ),
                    suspend: function.is_suspend() || preceded_by_suspend(node),
                    nullable: false,
                }
            }
            _ => TypeRef::Unreadable,
        }
    }
}

/// The range of the anonymous token `keyword` among `node`'s children.
fn keyword_range(node: SyntaxNode<'_>, keyword: &str) -> Option<TextRange> {
    let count = node.child_count();
    (0..count)
        .filter_map(|index| node.child(index as u32))
        .find(|child| !child.is_named() && child.kind() == keyword)
        .map(TextRange::of)
}

/// `suspend () -> T` puts `suspend` in a `type_modifiers` sibling before
/// the function type.
fn preceded_by_suspend(node: SyntaxNode<'_>) -> bool {
    syntax::previous_named_sibling(node)
        .filter(|sibling| sibling.kind() == "type_modifiers")
        .is_some_and(|modifiers| {
            syntax::descendants(modifiers).any(|token| token.kind() == "suspend")
        })
}
