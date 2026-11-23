//! Java syntax → [`ItemTree`]. Java is read for its declarations only: the
//! types Kotlin code can name and the members it can call.

use intern::{Interned, Name, Path};
use syntax::{Parse, SyntaxNode, TextRange, named_children};

use super::{
    ClassKind, Deprecation, Import, ImportTarget, Item, ItemId, ItemKind, ItemRanges,
    ItemSourceMap, ItemTree, ItemTreeBuilder, Modifiers, Parameter, Signature, TypeArgument,
    TypeParameter, TypeRef, TypeSegment, Variance, Visibility,
};

pub(crate) fn lower_java(parse: &Parse) -> (ItemTree, ItemSourceMap) {
    let mut lowering = Lowering {
        parse,
        builder: ItemTreeBuilder::default(),
    };
    let root = parse.root();
    let mut package = Path::default();
    let mut imports = Vec::new();
    let mut top_level = Vec::new();
    for node in named_children(root) {
        match node.kind() {
            "package_declaration" => {
                if let Some(name) = named_children(node)
                    .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
                {
                    package = Path::parse(lowering.text(name));
                }
            }
            "import_declaration" => imports.extend(lowering.import(node)),
            _ => top_level.extend(lowering.type_declaration(node, None, false)),
        }
    }
    lowering
        .builder
        .finish(package, imports, top_level, Vec::new())
}

struct Lowering<'parse> {
    parse: &'parse Parse,
    builder: ItemTreeBuilder,
}

impl<'parse> Lowering<'parse> {
    fn text(&self, node: SyntaxNode<'_>) -> &'parse str {
        &self.parse.text()[node.byte_range()]
    }

    fn name(&self, node: SyntaxNode<'_>) -> Name {
        Name::new(self.text(node))
    }

    /// An import and its path's range.
    fn import(&self, node: SyntaxNode<'_>) -> Option<(Import, TextRange)> {
        let path_node = named_children(node)
            .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))?;
        let everything = named_children(node).any(|child| child.kind() == "asterisk");
        let import = Import {
            path: Path::parse(self.text(path_node)),
            target: if everything {
                ImportTarget::Everything
            } else {
                ImportTarget::Single { alias: None }
            },
        };
        Some((import, TextRange::of(path_node)))
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        name: Name,
        kind: ItemKind,
        name_node: SyntaxNode<'_>,
        node: SyntaxNode<'_>,
        parent: Option<ItemId>,
        (visibility, modifiers): (Visibility, Modifiers),
        type_parameters: Box<[TypeParameter]>,
        signature: Signature,
    ) -> ItemId {
        let ranges = ItemRanges {
            range: TextRange::of(node),
            name_range: TextRange::of(name_node),
        };
        let item = Item {
            name,
            kind,
            parent,
            children: Box::new([]),
            visibility,
            modifiers,
            type_parameters,
            signature,
        };
        let id = self.builder.push(item, ranges);
        if modifiers.contains(Modifiers::DEPRECATED) || self.deprecated_in_javadoc(node) {
            self.builder.deprecate(id, Deprecation::java());
        }
        id
    }

    /// Whether the Javadoc right before `declaration` has a `@deprecated`
    /// tag: Kotlin counts it like the annotation.
    fn deprecated_in_javadoc(&self, declaration: SyntaxNode<'_>) -> bool {
        let Some(comment) = declaration.prev_sibling() else {
            return false;
        };
        let text = self.text(comment);
        comment.kind() == "block_comment" && text.starts_with("/**") && text.contains("@deprecated")
    }

    /// A class, interface, enum, record or annotation declaration.
    fn type_declaration(
        &mut self,
        node: SyntaxNode<'_>,
        parent: Option<ItemId>,
        in_interface: bool,
    ) -> Option<ItemId> {
        let kind = match node.kind() {
            "class_declaration" | "record_declaration" => ClassKind::Class,
            "interface_declaration" => ClassKind::Interface,
            "enum_declaration" => ClassKind::Enum,
            "annotation_type_declaration" => ClassKind::Annotation,
            _ => return None,
        };
        let name_node = node.child_by_field_name("name")?;
        let mut supertypes = Vec::new();
        if let Some(superclass) = node.child_by_field_name("superclass") {
            supertypes
                .extend(named_children(superclass).map(|type_node| self.interned_type(type_node)));
        }
        let interface_lists = node
            .child_by_field_name("interfaces")
            .into_iter()
            .chain(named_children(node).filter(|child| child.kind() == "extends_interfaces"));
        for list in interface_lists {
            for type_list in named_children(list).filter(|child| child.kind() == "type_list") {
                supertypes.extend(
                    named_children(type_list).map(|type_node| self.interned_type(type_node)),
                );
            }
        }
        let primary_constructor = (node.kind() == "record_declaration")
            .then(|| node.child_by_field_name("parameters"))
            .flatten()
            .map(|parameters| self.parameters(parameters));
        let type_parameters = self.type_parameters(node.child_by_field_name("type_parameters"));
        let modifiers = self.modifiers(node, in_interface);
        let index = self.push(
            self.name(name_node),
            ItemKind::Class(kind),
            name_node,
            node,
            parent,
            modifiers,
            type_parameters,
            Signature::Class {
                supertypes: supertypes.into_boxed_slice(),
                primary_constructor,
            },
        );
        let mut children = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            let members_in_interface = matches!(kind, ClassKind::Interface | ClassKind::Annotation);
            self.members(body, index, members_in_interface, &mut children);
        }
        self.builder.set_children(index, children);
        Some(index)
    }

    fn members(
        &mut self,
        body: SyntaxNode<'_>,
        parent: ItemId,
        in_interface: bool,
        children: &mut Vec<ItemId>,
    ) {
        for member in named_children(body) {
            match member.kind() {
                "enum_constant" => {
                    if let Some(name_node) = member.child_by_field_name("name") {
                        children.push(self.push(
                            self.name(name_node),
                            ItemKind::EnumEntry,
                            name_node,
                            member,
                            Some(parent),
                            (Visibility::Public, Modifiers::STATIC),
                            Box::new([]),
                            Signature::None,
                        ));
                    }
                }
                // An enum's other members follow its constants in here.
                "enum_body_declarations" => self.members(member, parent, in_interface, children),
                "method_declaration" | "annotation_type_element_declaration" => {
                    let Some(name_node) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let parameters = member
                        .child_by_field_name("parameters")
                        .map(|parameters| self.parameters(parameters))
                        .unwrap_or_default();
                    let return_type = member
                        .child_by_field_name("type")
                        .map(|node| self.interned_type(node));
                    let type_parameters =
                        self.type_parameters(member.child_by_field_name("type_parameters"));
                    let (visibility, mut kept) = self.modifiers(member, in_interface);
                    // An interface method without a body (not `default`, not
                    // `static`) is implicitly abstract.
                    let has_body = member.child_by_field_name("body").is_some();
                    if in_interface && !has_body && !kept.contains(Modifiers::STATIC) {
                        kept.insert(Modifiers::ABSTRACT);
                    }
                    let modifiers = (visibility, kept);
                    children.push(self.push(
                        self.name(name_node),
                        ItemKind::Function,
                        name_node,
                        member,
                        Some(parent),
                        modifiers,
                        type_parameters,
                        Signature::Function {
                            receiver: None,
                            parameters,
                            return_type,
                        },
                    ));
                }
                "constructor_declaration" | "compact_constructor_declaration" => {
                    let Some(name_node) = member.child_by_field_name("name") else {
                        continue;
                    };
                    let parameters = member
                        .child_by_field_name("parameters")
                        .map(|parameters| self.parameters(parameters))
                        .unwrap_or_default();
                    let modifiers = self.modifiers(member, in_interface);
                    children.push(self.push(
                        self.name(name_node),
                        ItemKind::Constructor,
                        name_node,
                        member,
                        Some(parent),
                        modifiers,
                        Box::new([]),
                        Signature::Function {
                            receiver: None,
                            parameters,
                            return_type: None,
                        },
                    ));
                }
                "field_declaration" | "constant_declaration" => {
                    let type_ref = member
                        .child_by_field_name("type")
                        .map(|node| self.interned_type(node));
                    let (visibility, mut modifiers) = self.modifiers(member, in_interface);
                    // Interface fields are implicitly `public static final`.
                    if in_interface {
                        modifiers.insert(Modifiers::STATIC);
                        modifiers.insert(Modifiers::FINAL);
                    }
                    let mutable = !modifiers.contains(Modifiers::FINAL);
                    let mut cursor = member.walk();
                    let declarators: Vec<SyntaxNode<'_>> = member
                        .children_by_field_name("declarator", &mut cursor)
                        .collect();
                    for declarator in declarators {
                        let Some(name_node) = declarator.child_by_field_name("name") else {
                            continue;
                        };
                        children.push(self.push(
                            self.name(name_node),
                            ItemKind::Property { mutable },
                            name_node,
                            member,
                            Some(parent),
                            (visibility, modifiers),
                            Box::new([]),
                            Signature::Property {
                                receiver: None,
                                type_ref,
                            },
                        ));
                    }
                }
                _ => {
                    children.extend(self.type_declaration(member, Some(parent), in_interface));
                }
            }
        }
    }

    fn parameters(&self, parameters: SyntaxNode<'_>) -> Box<[Parameter]> {
        named_children(parameters)
            .filter_map(|parameter| match parameter.kind() {
                "formal_parameter" => Some(Parameter {
                    name: self.name(parameter.child_by_field_name("name")?),
                    type_ref: parameter
                        .child_by_field_name("type")
                        .map(|node| self.interned_type(node)),
                    has_default: false,
                    vararg: false,
                }),
                "spread_parameter" => {
                    let declarator = named_children(parameter)
                        .find(|child| child.kind() == "variable_declarator")?;
                    let type_node = named_children(parameter).find(|child| {
                        !matches!(child.kind(), "modifiers" | "variable_declarator")
                            && !child.kind().contains("annotation")
                    });
                    Some(Parameter {
                        name: self.name(declarator.child_by_field_name("name")?),
                        type_ref: type_node.map(|node| self.interned_type(node)),
                        has_default: false,
                        vararg: true,
                    })
                }
                _ => None,
            })
            .collect()
    }

    fn type_parameters(&self, parameters: Option<SyntaxNode<'_>>) -> Box<[TypeParameter]> {
        let Some(parameters) = parameters else {
            return Box::new([]);
        };
        named_children(parameters)
            .filter(|parameter| parameter.kind() == "type_parameter")
            .filter_map(|parameter| {
                let name =
                    named_children(parameter).find(|child| child.kind() == "type_identifier")?;
                let bounds = named_children(parameter)
                    .filter(|child| child.kind() == "type_bound")
                    .flat_map(named_children)
                    .map(|bound| self.interned_type(bound))
                    .collect();
                Some(TypeParameter {
                    name: self.name(name),
                    bounds,
                    reified: false,
                    variance: Variance::Invariant,
                })
            })
            .collect()
    }

    /// Visibility (Java's default: the package; an interface's members:
    /// public) and kept modifiers.
    fn modifiers(
        &self,
        declaration: SyntaxNode<'_>,
        in_interface: bool,
    ) -> (Visibility, Modifiers) {
        let mut visibility = if in_interface {
            Visibility::Public
        } else {
            Visibility::PackagePrivate
        };
        let mut kept = Modifiers::default();
        let Some(modifiers) = named_children(declaration).find(|child| child.kind() == "modifiers")
        else {
            return (visibility, kept);
        };
        let count = modifiers.child_count();
        for word in (0..count).filter_map(|index| modifiers.child(index as u32)) {
            match word.kind() {
                "public" => visibility = Visibility::Public,
                "protected" => visibility = Visibility::Protected,
                "private" => visibility = Visibility::Private,
                "static" => kept.insert(Modifiers::STATIC),
                "abstract" => kept.insert(Modifiers::ABSTRACT),
                "final" => kept.insert(Modifiers::FINAL),
                "marker_annotation" | "annotation" if self.is_deprecation(word) => {
                    kept.insert(Modifiers::DEPRECATED);
                }
                _ => {}
            }
        }
        (visibility, kept)
    }

    fn is_deprecation(&self, annotation: SyntaxNode<'_>) -> bool {
        self.text(annotation)
            .trim_start_matches('@')
            .ends_with("Deprecated")
    }

    fn interned_type(&self, node: SyntaxNode<'_>) -> Interned<TypeRef> {
        self.type_ref(node).interned()
    }

    fn type_ref(&self, node: SyntaxNode<'_>) -> TypeRef {
        match node.kind() {
            "type_identifier" => TypeRef::Named {
                segments: Box::new([TypeSegment {
                    name: self.name(node),
                    arguments: Box::new([]),
                }]),
                nullable: false,
            },
            "scoped_type_identifier" | "generic_type" => {
                let mut segments = Vec::new();
                self.segments(node, &mut segments);
                TypeRef::Named {
                    segments: segments.into_boxed_slice(),
                    nullable: false,
                }
            }
            "array_type" => TypeRef::Array {
                element: node
                    .child_by_field_name("element")
                    .map(|element| self.interned_type(element))
                    .unwrap_or_else(|| TypeRef::Unreadable.interned()),
            },
            "integral_type" | "floating_point_type" | "boolean_type" | "void_type" => {
                TypeRef::Primitive {
                    name: Name::new(self.text(node)),
                }
            }
            _ => TypeRef::Unreadable,
        }
    }

    /// The segments of `a.b.Outer<X>.Inner`, outermost first.
    fn segments(&self, node: SyntaxNode<'_>, segments: &mut Vec<TypeSegment>) {
        for child in named_children(node) {
            match child.kind() {
                "type_identifier" => segments.push(TypeSegment {
                    name: self.name(child),
                    arguments: Box::new([]),
                }),
                "scoped_type_identifier" | "generic_type" => self.segments(child, segments),
                "type_arguments" => {
                    let arguments: Box<[TypeArgument]> = named_children(child)
                        .map(|argument| {
                            if argument.kind() == "wildcard" {
                                return self.wildcard(argument);
                            }
                            TypeArgument::Type {
                                variance: Variance::Invariant,
                                type_ref: self.interned_type(argument),
                            }
                        })
                        .collect();
                    if let Some(last) = segments.last_mut() {
                        last.arguments = arguments;
                    }
                }
                _ => {}
            }
        }
    }

    /// `?`, `? extends T`, `? super T`.
    fn wildcard(&self, node: SyntaxNode<'_>) -> TypeArgument {
        // `super` is a named node in this grammar, `extends` a token.
        let is_super = named_children(node).any(|child| child.kind() == "super")
            || syntax::has_token(node, "super");
        let bound = named_children(node)
            .filter(|child| !child.kind().contains("annotation") && child.kind() != "super")
            .last();
        let Some(bound) = bound else {
            return TypeArgument::Star;
        };
        TypeArgument::Type {
            variance: if is_super {
                Variance::In
            } else {
                Variance::Out
            },
            type_ref: self.interned_type(bound),
        }
    }
}
