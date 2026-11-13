//! Kotlin's declarations and types, as the tree-sitter Kotlin grammar shapes
//! them. The grammar has few named fields: parts are told apart by kind and
//! by order (a function's return type is the type after its parameters).

use super::{AstNode, ast_nodes, child, children};
use crate::{SyntaxNode, has_token, named_children};

ast_nodes! {
    SourceFile => "source_file",
    PackageHeader => "package_header",
    ImportList => "import_list",
    ImportHeader => "import_header",
    /// A dotted name: `com.example.Foo` in a package or import header.
    Identifier => "identifier",
    ClassDeclaration => "class_declaration",
    ObjectDeclaration => "object_declaration",
    CompanionObject => "companion_object",
    FunctionDeclaration => "function_declaration",
    PropertyDeclaration => "property_declaration",
    TypeAlias => "type_alias",
    SecondaryConstructor => "secondary_constructor",
    EnumEntry => "enum_entry",
    PrimaryConstructor => "primary_constructor",
    ClassParameter => "class_parameter",
    FunctionValueParameters => "function_value_parameters",
    Parameter => "parameter",
    VariableDeclaration => "variable_declaration",
    TypeParameters => "type_parameters",
    TypeParameter => "type_parameter",
    DelegationSpecifier => "delegation_specifier",
    Modifiers => "modifiers",
    UserType => "user_type",
    TypeArguments => "type_arguments",
    TypeProjection => "type_projection",
    FunctionType => "function_type",
}

/// A declaration that can appear at the top level or in a class body.
#[derive(Clone, Copy, Debug)]
pub enum Declaration<'tree> {
    Class(ClassDeclaration<'tree>),
    Object(ObjectDeclaration<'tree>),
    Companion(CompanionObject<'tree>),
    Function(FunctionDeclaration<'tree>),
    Property(PropertyDeclaration<'tree>),
    TypeAlias(TypeAlias<'tree>),
    SecondaryConstructor(SecondaryConstructor<'tree>),
    EnumEntry(EnumEntry<'tree>),
}

impl<'tree> Declaration<'tree> {
    pub fn cast(node: SyntaxNode<'tree>) -> Option<Self> {
        let declaration = match node.kind() {
            ClassDeclaration::KIND => Declaration::Class(ClassDeclaration::new_unchecked(node)),
            ObjectDeclaration::KIND => Declaration::Object(ObjectDeclaration::new_unchecked(node)),
            CompanionObject::KIND => Declaration::Companion(CompanionObject::new_unchecked(node)),
            FunctionDeclaration::KIND => {
                Declaration::Function(FunctionDeclaration::new_unchecked(node))
            }
            PropertyDeclaration::KIND => {
                Declaration::Property(PropertyDeclaration::new_unchecked(node))
            }
            TypeAlias::KIND => Declaration::TypeAlias(TypeAlias::new_unchecked(node)),
            SecondaryConstructor::KIND => {
                Declaration::SecondaryConstructor(SecondaryConstructor::new_unchecked(node))
            }
            EnumEntry::KIND => Declaration::EnumEntry(EnumEntry::new_unchecked(node)),
            _ => return None,
        };
        Some(declaration)
    }

    pub fn syntax(self) -> SyntaxNode<'tree> {
        match self {
            Declaration::Class(node) => node.syntax(),
            Declaration::Object(node) => node.syntax(),
            Declaration::Companion(node) => node.syntax(),
            Declaration::Function(node) => node.syntax(),
            Declaration::Property(node) => node.syntax(),
            Declaration::TypeAlias(node) => node.syntax(),
            Declaration::SecondaryConstructor(node) => node.syntax(),
            Declaration::EnumEntry(node) => node.syntax(),
        }
    }
}

/// The declarations directly in a container (a source file or a body).
fn declarations_in(node: SyntaxNode<'_>) -> impl Iterator<Item = Declaration<'_>> {
    named_children(node).filter_map(Declaration::cast)
}

/// The members of a class-like declaration's body.
fn members_of(declaration: SyntaxNode<'_>) -> impl Iterator<Item = Declaration<'_>> {
    named_children(declaration)
        .filter(|child| matches!(child.kind(), "class_body" | "enum_class_body"))
        .flat_map(declarations_in)
}

impl<'tree> SourceFile<'tree> {
    pub fn package_header(self) -> Option<PackageHeader<'tree>> {
        child(self.0)
    }

    pub fn imports(self) -> impl Iterator<Item = ImportHeader<'tree>> {
        let listed = children::<ImportList>(self.0).flat_map(|list| children(list.0));
        listed.chain(children::<ImportHeader>(self.0))
    }

    pub fn declarations(self) -> impl Iterator<Item = Declaration<'tree>> {
        declarations_in(self.0)
    }
}

impl<'tree> PackageHeader<'tree> {
    pub fn path(self) -> Option<Identifier<'tree>> {
        child(self.0)
    }
}

impl<'tree> Identifier<'tree> {
    /// The `simple_identifier`s of the dotted name, in order.
    pub fn segments(self) -> impl Iterator<Item = SyntaxNode<'tree>> {
        named_children(self.0).filter(|segment| segment.kind() == "simple_identifier")
    }
}

impl<'tree> ImportHeader<'tree> {
    pub fn path(self) -> Option<Identifier<'tree>> {
        child(self.0)
    }

    /// The `type_identifier` of `as Alias`.
    pub fn alias(self) -> Option<SyntaxNode<'tree>> {
        let alias = named_children(self.0).find(|child| child.kind() == "import_alias")?;
        named_children(alias).find(|child| child.kind() == "type_identifier")
    }

    /// `import a.b.*`.
    pub fn is_wildcard(self) -> bool {
        named_children(self.0).any(|child| child.kind() == "wildcard_import")
    }
}

/// What a `class_declaration` declares.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassKeyword {
    Class,
    Interface,
    /// `fun interface`.
    FunInterface,
    /// `enum class`.
    Enum,
}

impl<'tree> ClassDeclaration<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "type_identifier")
    }

    pub fn keyword(self) -> ClassKeyword {
        if has_token(self.0, "interface") {
            if has_token(self.0, "fun") {
                return ClassKeyword::FunInterface;
            }
            return ClassKeyword::Interface;
        }
        if has_token(self.0, "enum") {
            return ClassKeyword::Enum;
        }
        ClassKeyword::Class
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn type_parameters(self) -> Option<TypeParameters<'tree>> {
        child(self.0)
    }

    pub fn primary_constructor(self) -> Option<PrimaryConstructor<'tree>> {
        child(self.0)
    }

    pub fn supertypes(self) -> impl Iterator<Item = DelegationSpecifier<'tree>> {
        children(self.0)
    }

    pub fn members(self) -> impl Iterator<Item = Declaration<'tree>> {
        members_of(self.0)
    }
}

impl<'tree> ObjectDeclaration<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "type_identifier")
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn supertypes(self) -> impl Iterator<Item = DelegationSpecifier<'tree>> {
        children(self.0)
    }

    pub fn members(self) -> impl Iterator<Item = Declaration<'tree>> {
        members_of(self.0)
    }
}

impl<'tree> CompanionObject<'tree> {
    /// The name after `companion object`, when it has one.
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "type_identifier")
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn supertypes(self) -> impl Iterator<Item = DelegationSpecifier<'tree>> {
        children(self.0)
    }

    pub fn members(self) -> impl Iterator<Item = Declaration<'tree>> {
        members_of(self.0)
    }
}

impl<'tree> FunctionDeclaration<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "simple_identifier")
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn type_parameters(self) -> Option<TypeParameters<'tree>> {
        child(self.0)
    }

    /// `Receiver` of `fun Receiver.name()`.
    pub fn receiver(self) -> Option<SyntaxNode<'tree>> {
        receiver_type(self.0)
    }

    pub fn parameters(self) -> Option<FunctionValueParameters<'tree>> {
        child(self.0)
    }

    /// The type after the parameters, `: Type`.
    pub fn return_type(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0)
            .skip_while(|child| child.kind() != FunctionValueParameters::KIND)
            .skip(1)
            .find(|child| is_type(*child))
    }

    pub fn has_body(self) -> bool {
        named_children(self.0).any(|child| child.kind() == "function_body")
    }
}

/// The type inside a `receiver_type` child of a declaration.
fn receiver_type(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let receiver = named_children(node).find(|child| child.kind() == "receiver_type")?;
    named_children(receiver).find(|child| is_type(*child))
}

/// Which way a type test (`x is T`, `x !is T`, a `when` branch's `is T`)
/// goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeTest {
    Is,
    IsNot,
    /// Not a type test (`x in range`).
    None,
}

impl TypeTest {
    /// The test `node` (a `check_expression` or `type_test`) makes. `!is`
    /// is two tokens, `!` then `is`.
    pub fn of(node: SyntaxNode<'_>) -> TypeTest {
        let mut previous_was_bang = false;
        for child in crate::children(node).filter(|child| !child.is_named()) {
            match child.kind() {
                "is" if previous_was_bang => return TypeTest::IsNot,
                "is" => return TypeTest::Is,
                "!" => previous_was_bang = true,
                _ => previous_was_bang = false,
            }
        }
        TypeTest::None
    }
}

/// Whether `node` is a type: a reference, a nullable or function type.
pub fn is_type(node: SyntaxNode<'_>) -> bool {
    matches!(
        node.kind(),
        "user_type"
            | "nullable_type"
            | "function_type"
            | "parenthesized_type"
            | "not_nullable_type"
    )
}

/// One parameter of a function, with what surrounds it in the list.
#[derive(Clone, Copy, Debug)]
pub struct ParameterEntry<'tree> {
    pub parameter: Parameter<'tree>,
    pub vararg: bool,
    pub has_default: bool,
}

impl<'tree> FunctionValueParameters<'tree> {
    /// The parameters in order. The grammar puts a parameter's modifiers
    /// before it and its default value after it, as siblings.
    pub fn entries(self) -> Vec<ParameterEntry<'tree>> {
        let mut entries: Vec<ParameterEntry<'tree>> = Vec::new();
        let mut vararg = false;
        for node in named_children(self.0) {
            match node.kind() {
                "parameter_modifiers" => {
                    vararg = named_children(node).any(|modifier| has_token(modifier, "vararg"));
                }
                "parameter" => {
                    entries.push(ParameterEntry {
                        parameter: Parameter::new_unchecked(node),
                        vararg,
                        has_default: false,
                    });
                    vararg = false;
                }
                _ => {
                    // An expression after a parameter is its default value.
                    if let Some(last) = entries.last_mut() {
                        last.has_default = true;
                    }
                }
            }
        }
        entries
    }
}

impl<'tree> Parameter<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "simple_identifier")
    }

    pub fn type_node(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }
}

impl<'tree> PropertyDeclaration<'tree> {
    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn type_parameters(self) -> Option<TypeParameters<'tree>> {
        child(self.0)
    }

    pub fn receiver(self) -> Option<SyntaxNode<'tree>> {
        receiver_type(self.0)
    }

    /// `var` rather than `val`.
    pub fn is_mutable(self) -> bool {
        is_var_binding(self.0)
    }

    /// The declared variable; `None` for a destructuring declaration.
    pub fn variable(self) -> Option<VariableDeclaration<'tree>> {
        child(self.0)
    }
}

/// Whether a declaration's `binding_pattern_kind` child says `var`.
fn is_var_binding(node: SyntaxNode<'_>) -> bool {
    named_children(node)
        .find(|child| child.kind() == "binding_pattern_kind")
        .is_some_and(|binding| has_token(binding, "var"))
}

impl<'tree> VariableDeclaration<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "simple_identifier")
    }

    pub fn type_node(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }
}

impl<'tree> TypeAlias<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "type_identifier")
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn type_parameters(self) -> Option<TypeParameters<'tree>> {
        child(self.0)
    }

    pub fn target(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }
}

impl<'tree> SecondaryConstructor<'tree> {
    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn parameters(self) -> Option<FunctionValueParameters<'tree>> {
        child(self.0)
    }
}

impl<'tree> EnumEntry<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "simple_identifier")
    }
}

/// Whether a class parameter also declares a property.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParameterBinding {
    /// A plain constructor parameter.
    None,
    Val,
    Var,
}

impl<'tree> PrimaryConstructor<'tree> {
    pub fn parameters(self) -> impl Iterator<Item = ClassParameter<'tree>> {
        children(self.0)
    }
}

impl<'tree> ClassParameter<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "simple_identifier")
    }

    pub fn modifiers(self) -> Option<Modifiers<'tree>> {
        child(self.0)
    }

    pub fn binding(self) -> ParameterBinding {
        let Some(binding) =
            named_children(self.0).find(|child| child.kind() == "binding_pattern_kind")
        else {
            return ParameterBinding::None;
        };
        if has_token(binding, "var") {
            ParameterBinding::Var
        } else {
            ParameterBinding::Val
        }
    }

    pub fn type_node(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }

    pub fn is_vararg(self) -> bool {
        self.modifiers().is_some_and(|modifiers| {
            modifiers
                .words()
                .any(|word| word.kind() == "parameter_modifier" && has_token(word, "vararg"))
        })
    }

    /// Whether `= value` follows the type.
    pub fn has_default(self) -> bool {
        named_children(self.0)
            .skip_while(|child| !is_type(*child))
            .nth(1)
            .is_some()
    }
}

impl<'tree> TypeParameters<'tree> {
    pub fn parameters(self) -> impl Iterator<Item = TypeParameter<'tree>> {
        children(self.0)
    }
}

impl<'tree> TypeParameter<'tree> {
    pub fn name(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| child.kind() == "type_identifier")
    }

    /// `Bound` of `T : Bound`.
    pub fn bound(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }
}

impl<'tree> DelegationSpecifier<'tree> {
    /// The supertype: `Base` of `Base(1)`, `Iface`, `Iface by delegate`.
    pub fn type_node(self) -> Option<SyntaxNode<'tree>> {
        for child in named_children(self.0) {
            match child.kind() {
                "constructor_invocation" | "explicit_delegation" => {
                    return named_children(child).find(|inner| is_type(*inner));
                }
                _ if is_type(child) => return Some(child),
                _ => {}
            }
        }
        None
    }
}

impl<'tree> Modifiers<'tree> {
    /// The modifier nodes (`visibility_modifier`, `class_modifier`, …) and
    /// annotations, in order.
    pub fn words(self) -> impl Iterator<Item = SyntaxNode<'tree>> {
        named_children(self.0)
    }
}

impl<'tree> UserType<'tree> {
    /// `(name, type arguments)` per segment of `a.b.Outer<X>.Inner<Y>`.
    pub fn segments(self) -> Vec<(SyntaxNode<'tree>, Option<TypeArguments<'tree>>)> {
        let mut segments: Vec<(SyntaxNode<'tree>, Option<TypeArguments<'tree>>)> = Vec::new();
        for child in named_children(self.0) {
            match child.kind() {
                "type_identifier" => segments.push((child, None)),
                TypeArguments::KIND => {
                    if let Some(last) = segments.last_mut() {
                        last.1 = Some(TypeArguments::new_unchecked(child));
                    }
                }
                _ => {}
            }
        }
        segments
    }
}

impl<'tree> TypeArguments<'tree> {
    pub fn projections(self) -> impl Iterator<Item = TypeProjection<'tree>> {
        children(self.0)
    }
}

/// `in` / `out` on a type argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variance {
    Invariant,
    In,
    Out,
}

impl<'tree> TypeProjection<'tree> {
    /// `None` for the star projection `*`.
    pub fn type_node(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0).find(|child| is_type(*child))
    }

    pub fn variance(self) -> Variance {
        let modifiers =
            named_children(self.0).find(|child| child.kind() == "type_projection_modifiers");
        let Some(modifiers) = modifiers else {
            return Variance::Invariant;
        };
        let variance =
            crate::descendants(modifiers).find(|node| node.kind() == "variance_modifier");
        match variance {
            Some(node) if has_token(node, "in") => Variance::In,
            Some(node) if has_token(node, "out") => Variance::Out,
            _ => Variance::Invariant,
        }
    }
}

impl<'tree> FunctionType<'tree> {
    pub fn receiver(self) -> Option<SyntaxNode<'tree>> {
        receiver_type(self.0)
    }

    /// The parameter types of `(A, B) -> R`.
    pub fn parameters(self) -> Vec<SyntaxNode<'tree>> {
        named_children(self.0)
            .find(|child| child.kind() == "function_type_parameters")
            .map(|parameters| {
                named_children(parameters)
                    .map(|parameter| {
                        // A named function type parameter: `(name: Type)`.
                        if parameter.kind() == "parameter" {
                            named_children(parameter)
                                .find(|inner| is_type(*inner))
                                .unwrap_or(parameter)
                        } else {
                            parameter
                        }
                    })
                    .filter(|parameter| is_type(*parameter))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The result type, after `->`.
    pub fn result(self) -> Option<SyntaxNode<'tree>> {
        named_children(self.0)
            .filter(|child| is_type(*child))
            .last()
    }

    pub fn is_suspend(self) -> bool {
        named_children(self.0)
            .filter(|child| child.kind() == "type_modifiers")
            .any(|modifiers| crate::descendants(modifiers).any(|node| has_token(node, "suspend")))
    }
}
