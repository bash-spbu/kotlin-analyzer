//! Expressions: what each evaluates to, and which declaration each name in
//! it means.

use hir_def::ItemLocation;
use hir_def::item_tree::{ItemKind, Signature};
use hir_def::resolver::Resolution;
use intern::{Name, Path};
use syntax::ast::kotlin::TypeTest;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::{Inferrer, SuperOf};
use super::flow::Flow;
use super::narrowing::{CallEnd, Narrowed};
use super::property_paths::{PathRoot, ReceiverRoot};
use super::reassociation::{self, Operation};
use super::type_mismatches::{Expectation, Requirement};
use super::{LocalKind, NameSite, NullCheckKind, Reference};
use crate::signature::Callable;
use crate::ty::{FunctionType, Nullability, Ty};

/// What a `when` branch's conditions test.
#[derive(Clone, Copy)]
struct WhenSubject<'subject> {
    /// The local or stable property the subject is (`when (x)`), to
    /// smart-cast.
    local: Option<Narrowed>,
    ty: Option<&'subject Ty>,
    /// Whether the subject's type is the one the compiler gives it (see
    /// `examinable`).
    examinable: bool,
    /// What a branch matching a value that can't be null says isn't null
    /// (`existedOrder` of `when (existedOrder?.status)`).
    not_null_when_matched: &'subject [(Narrowed, Ty)],
}

/// One branch of a `when`: its value's type, whether it is `else`, the
/// flow it ends with, and what the branches after it know (its conditions
/// were false).
struct WhenBranch {
    ty: Ty,
    is_else: bool,
    end: Flow,
    when_false: Vec<(Narrowed, Ty)>,
}

/// What an expression denotes: a value, or — as the qualifier before a
/// `.` — a class or a package.
#[derive(Clone, Debug)]
pub(super) enum Typed {
    Value(Ty),
    Class(ItemLocation),
    Package(Path),
}

impl Inferrer<'_> {
    /// The type of `node`, given the type its context expects (if any).
    pub(super) fn expression(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let typed = self.typed(node, expected);
        self.value_of(typed)
    }

    /// A class name as a value is its object, or its companion object.
    pub(super) fn value_of(&self, typed: Typed) -> Ty {
        match typed {
            Typed::Value(ty) => ty,
            Typed::Class(class) => self
                .types
                .object_instance(class)
                .map(Ty::Class)
                .unwrap_or(Ty::Unknown),
            Typed::Package(_) => Ty::Unknown,
        }
    }

    pub(super) fn typed(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Typed {
        self.typed_with(node, Expectation::hint(expected))
    }

    /// `node`, given what the place it goes says of it.
    pub(super) fn typed_with(
        &mut self,
        node: SyntaxNode<'_>,
        expectation: Expectation<'_>,
    ) -> Typed {
        if self.depth >= super::declarations::MAX_DEPTH {
            return Typed::Value(Ty::Unknown);
        }
        self.depth += 1;
        let typed = match expectation.requirement {
            Some(requirement) => self.typed_required(node, expectation.hint, requirement),
            None => self.typed_inner(node, expectation.hint),
        };
        self.depth -= 1;
        typed
    }

    /// A branching expression whose branches' values must meet
    /// `requirement`: each is checked where it ends.
    pub(super) fn branching(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: &Requirement,
    ) -> Ty {
        let requirement = Some(requirement.clone());
        match node.kind() {
            "if_expression" => self.if_expression(node, expected, requirement),
            "when_expression" => self.when_expression(node, expected, requirement),
            "try_expression" => self.try_expression(node, expected, requirement),
            "elvis_expression" => self.elvis(node, expected, requirement),
            _ => self.expression(node, expected),
        }
    }

    pub(super) fn typed_inner(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Typed {
        if let Some(chain) = reassociation::misparsed_chain(node) {
            return Typed::Value(self.reassociated(node, chain, expected));
        }
        self.typed_by_kind(node, expected)
    }

    /// `leading <operation> chain`, for a chain the grammar based on the
    /// whole operation.
    fn reassociated(
        &mut self,
        node: SyntaxNode<'_>,
        chain: reassociation::MisparsedChain<'_>,
        expected: Option<&Ty>,
    ) -> Ty {
        let leading = self.expression(chain.leading, None);
        match chain.operation {
            Operation::Operator(function) => {
                let right = self.value_of_kind(node, None);
                self.operator_result(&leading, function, vec![right])
                    .unwrap_or(Ty::Unknown)
            }
            Operation::Comparison => {
                self.value_of_kind(node, None);
                self.types.builtin("Boolean")
            }
            Operation::Elvis => {
                let right = self.value_of_kind(node, expected);
                self.types.common_type(&[leading.non_null(), right])
            }
        }
    }

    fn value_of_kind(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let typed = self.typed_by_kind(node, expected);
        self.value_of(typed)
    }

    fn typed_by_kind(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Typed {
        let value = match node.kind() {
            "simple_identifier" => return self.identifier(node),
            "navigation_expression" => return self.navigation(node),
            "parenthesized_expression" => match named_children(node).next() {
                Some(inner) => return self.typed(inner, expected),
                None => Ty::Unknown,
            },
            "call_expression" => {
                let ty = self.call(node, expected);
                // `requireNotNull(x)` anywhere — an argument, a receiver,
                // an initializer: `x` isn't null once it returned.
                match self.narrow_after_contract(node) {
                    CallEnd::Returns => ty,
                    CallEnd::Never => Ty::Nothing { nullable: false },
                }
            }
            "integer_literal" | "hex_literal" | "bin_literal" => self.types.builtin("Int"),
            "long_literal" => self.types.builtin("Long"),
            "real_literal" => {
                let text = self.text(node);
                if text.ends_with('f') || text.ends_with('F') {
                    self.types.builtin("Float")
                } else {
                    self.types.builtin("Double")
                }
            }
            "unsigned_literal" => {
                let text = self.text(node);
                if text.ends_with("uL") || text.ends_with("UL") {
                    self.types.builtin("ULong")
                } else {
                    self.types.builtin("UInt")
                }
            }
            "boolean_literal" => self.types.builtin("Boolean"),
            "character_literal" => self.types.builtin("Char"),
            "null_literal" => Ty::Nothing { nullable: true },
            "string_literal" | "multiline_string_literal" | "line_string_literal" => {
                self.string_template(node);
                self.types.builtin("String")
            }
            "this_expression" => self.this(node),
            "super_expression" => self.super_type(node),
            "prefix_expression" => self.prefix(node, expected),
            "postfix_expression" => self.postfix(node),
            "as_expression" => self.cast(node),
            "check_expression"
            | "equality_expression"
            | "comparison_expression"
            | "conjunction_expression"
            | "disjunction_expression" => {
                self.condition(node);
                self.types.builtin("Boolean")
            }
            "additive_expression" | "multiplicative_expression" | "range_expression" => {
                self.binary_operator(node)
            }
            "infix_expression" => self.infix(node),
            "elvis_expression" => self.elvis(node, expected, None),
            "indexing_expression" => self.indexing(node),
            "if_expression" => self.if_expression(node, expected, None),
            "when_expression" => self.when_expression(node, expected, None),
            "try_expression" => self.try_expression(node, expected, None),
            "jump_expression" => {
                self.jump(node);
                Ty::Nothing { nullable: false }
            }
            "lambda_literal" => self.lambda(node, expected, None),
            "annotated_lambda" => {
                match named_children(node).find(|child| child.kind() == "lambda_literal") {
                    Some(lambda) => self.lambda(lambda, expected, None),
                    None => Ty::Unknown,
                }
            }
            "anonymous_function" => self.anonymous_function(node),
            "callable_reference" => self.callable_reference(node),
            "object_literal" => self.object_literal(node),
            "assignment" => {
                self.assignment(node);
                self.types.builtin("Unit")
            }
            "for_statement" => {
                self.for_statement(node);
                self.types.builtin("Unit")
            }
            "while_statement" | "do_while_statement" => self.while_statement(node),
            "spread_expression" | "annotated_expression" | "labeled_expression" => {
                match named_children(node).last() {
                    Some(inner) => self.expression(inner, expected),
                    None => Ty::Unknown,
                }
            }
            "statements" | "control_structure_body" => self.body(node, expected),
            _ => {
                for child in named_children(node) {
                    self.expression(child, None);
                }
                Ty::Unknown
            }
        };
        Typed::Value(value)
    }

    /// A branch body: a block of statements, or a single expression.
    pub(super) fn body(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let content: Vec<SyntaxNode<'_>> = named_children(node).collect();
        match content[..] {
            [] => self.types.builtin("Unit"),
            [only] if only.kind() == "statements" => self.block(only),
            [only] if node.kind() == "control_structure_body" => {
                self.push_scope();
                let ty = match only.kind() {
                    "property_declaration" | "function_declaration" | "class_declaration" => {
                        self.statement(only)
                    }
                    _ => self.expression(only, expected),
                };
                self.pop_scope();
                ty
            }
            _ => self.block(node),
        }
    }

    // --- names ------------------------------------------------------------

    /// A name used as a value: a local, a property of an implicit receiver,
    /// a top-level property, an object or class (as a qualifier), or a
    /// package.
    pub(super) fn identifier(&mut self, node: SyntaxNode<'_>) -> Typed {
        if self.probed(node) {
            self.capture(None);
        }
        let name = self.name(node);
        if let Some(local) = self.lookup_value_local(name) {
            let ty = self.local_type(local);
            self.record(node, Reference::Local(local), ty.clone());
            return Typed::Value(ty);
        }
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receiver_type(index);
            if let Some((location, ty)) = self.property_of(&receiver, name) {
                let root = Self::implicit_root(index);
                let ty = self.read_property(node, root, location, ty);
                self.record(node, Reference::Declaration(location), ty.clone());
                return Typed::Value(ty);
            }
            if let Some(classifier) = self.receiver_classifier(&receiver, name) {
                self.record(node, Reference::Declaration(classifier), Ty::Unknown);
                return Typed::Class(classifier);
            }
        }
        if let Some((class, entries)) = self.enum_entries_in_scope(name) {
            self.record(node, Reference::Declaration(class), entries.clone());
            return Typed::Value(entries);
        }
        if let Some((location, ty)) = self.top_level_property(name) {
            let root = ReceiverRoot::Stable(PathRoot::Static);
            let ty = self.read_property(node, root, location, ty);
            self.record(node, Reference::Declaration(location), ty.clone());
            return Typed::Value(ty);
        }
        let path = Path::new([name]);
        match self.resolve_type_path_at(&path, node.start_byte()) {
            Resolution::Declaration(class) => {
                let ty = self.value_of(Typed::Class(class));
                self.record(node, Reference::Declaration(class), ty);
                return Typed::Class(class);
            }
            Resolution::Ambiguous(candidates) => {
                self.record(node, Reference::Candidates(candidates.into()), Ty::Unknown);
                return Typed::Value(Ty::Unknown);
            }
            _ => {}
        }
        if hir_def::def_map::package_prefix_exists(self.db, self.file, &path) {
            return Typed::Package(path);
        }
        // A function's name where a value goes: a reference Kotlin rejects —
        // or a call the parser split (`returns mockk<T> { … }`). Either way
        // the name is the function's.
        let functions = self.functions_named(name);
        if !functions.is_empty() {
            self.record(node, Reference::Candidates(functions.into()), Ty::Unknown);
            return Typed::Value(Ty::Unknown);
        }
        Typed::Value(self.record_unresolved(node, NameSite::Scope))
    }

    /// The functions named `name` an unqualified call could reach: members
    /// of the implicit receivers, then top-level ones.
    fn functions_named(&mut self, name: Name) -> Vec<ItemLocation> {
        let mut found: Vec<ItemLocation> = Vec::new();
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receiver_type(index);
            found.extend(
                self.types
                    .members_named(&receiver, name)
                    .into_iter()
                    .map(|member| member.location)
                    .filter(|location| self.item_kind(*location) == ItemKind::Function),
            );
        }
        let levels = self.top_level(name);
        for level in levels.iter() {
            found.extend(
                level
                    .iter()
                    .copied()
                    .filter(|location| self.item_kind(*location) == ItemKind::Function),
            );
        }
        found
    }

    /// A nested classifier or enum entry visible by its simple name inside
    /// a class (`Status.ACTIVE` written as `ACTIVE` inside `Status`).
    fn receiver_classifier(&self, receiver: &Ty, name: Name) -> Option<ItemLocation> {
        let class = receiver.class()?.class;
        self.types
            .static_members_named(class, name)
            .into_iter()
            .find(|member| {
                self.item_kind(*member).is_classifier()
                    || self.item_kind(*member) == ItemKind::EnumEntry
            })
    }

    /// The property `name` of a value of type `receiver` — a member, a
    /// Java getter, or an extension property — and its type there.
    pub(super) fn property_of(&mut self, receiver: &Ty, name: Name) -> Option<(ItemLocation, Ty)> {
        let members = self.types.members_named(receiver, name);
        let properties: Vec<(ItemLocation, Ty)> = members
            .iter()
            .filter(|member| matches!(self.item_kind(member.location), ItemKind::Property { .. }))
            .map(|member| {
                let ty = self
                    .types
                    .property_type(member.location)
                    .substitute(&member.substitution);
                (member.location, ty)
            })
            .collect();
        if let Some((location, _)) = properties.first() {
            return Some((*location, self.intersection_override_type(&properties)));
        }
        if let Some(getter) = self.types.java_getters(receiver, name).into_iter().next() {
            let ty = self
                .types
                .signature(Callable::Function(getter.location))
                .map(|signature| signature.result.substitute(&getter.substitution))
                .unwrap_or(Ty::Unknown);
            return Some((getter.location, ty));
        }
        self.extension_property(receiver, name)
    }

    /// The type of a property several supertypes declare (none overriding
    /// another): the most specific of theirs, as Kotlin's intersection
    /// override has — `IrSimpleFunctionSymbol.owner` is an
    /// `IrSimpleFunction`, not the `IrFunction` of `IrFunctionSymbol`.
    /// Unknown when none is a subtype of all the others.
    fn intersection_override_type(&self, properties: &[(ItemLocation, Ty)]) -> Ty {
        let types: Vec<&Ty> = properties.iter().map(|(_, ty)| ty).collect();
        let most_specific = types.iter().find(|candidate| {
            types
                .iter()
                .all(|other| self.types.fits(candidate, other) == super::Fit::Yes)
        });
        match (types.as_slice(), most_specific) {
            ([only], _) => (*only).clone(),
            (_, Some(most_specific)) => (**most_specific).clone(),
            (_, None) => Ty::Unknown,
        }
    }

    fn extension_property(&mut self, receiver: &Ty, name: Name) -> Option<(ItemLocation, Ty)> {
        let levels = self.top_level(name);
        for level in levels.iter() {
            for location in level {
                if !matches!(self.item_kind(*location), ItemKind::Property { .. }) {
                    continue;
                }
                let Some(declared_receiver) = self.types.property_receiver(*location) else {
                    continue;
                };
                if self
                    .types
                    .fits(&receiver.clone().non_null(), &declared_receiver)
                    == super::Fit::No
                {
                    continue;
                }
                let parameters: Vec<crate::ty::TypeParameterId> =
                    (0..hir_def::item_tree(self.db, location.container)
                        .item(location.item)
                        .type_parameters
                        .len())
                        .map(|index| crate::ty::TypeParameterId {
                            owner: *location,
                            index: index as u32,
                        })
                        .collect();
                let mut substitution = crate::ty::Substitution::default();
                self.types
                    .unify(&declared_receiver, receiver, &parameters, &mut substitution);
                let ty = self
                    .types
                    .property_type(*location)
                    .substitute(&substitution);
                return Some((*location, super::call::erase(&ty, &parameters)));
            }
        }
        None
    }

    /// A top-level property (or an explicitly imported object member).
    fn top_level_property(&mut self, name: Name) -> Option<(ItemLocation, Ty)> {
        let levels = self.top_level(name);
        for level in levels.iter() {
            let found = level.iter().find(|location| {
                let tree = hir_def::item_tree(self.db, location.container);
                let item = tree.item(location.item);
                let is_plain_property = matches!(
                    (&item.kind, &item.signature),
                    (
                        ItemKind::Property { .. },
                        Signature::Property { receiver: None, .. }
                    )
                );
                is_plain_property || item.kind == ItemKind::EnumEntry
            });
            if let Some(location) = found {
                return Some((*location, self.types.property_type(*location)));
            }
        }
        None
    }

    /// `receiver.name` and `receiver?.name` used as a value.
    fn navigation(&mut self, node: SyntaxNode<'_>) -> Typed {
        let mut children = named_children(node);
        let (Some(receiver_node), Some(suffix)) = (children.next(), children.next()) else {
            return Typed::Value(Ty::Unknown);
        };
        let receiver_node = reassociation::chain_operand(receiver_node).unwrap_or(receiver_node);
        let name_node = named_children(suffix).find(|child| child.kind() == "simple_identifier");
        let member = name_node.map(|name| self.name(name));
        let receiver = self.member_receiver(receiver_node, member);
        let safe = has_token(suffix, "?.");
        self.check_safe_call(receiver_node, &receiver, suffix);
        let Some(name_node) = name_node else {
            // `Foo::class` and the like.
            return Typed::Value(Ty::Unknown);
        };
        if self.probed(name_node) {
            self.capture(Some(&receiver));
        }
        let name = self.name(name_node);
        if has_token(suffix, "::") {
            return Typed::Value(self.member_reference(&receiver, name_node, name));
        }
        match receiver {
            Typed::Value(receiver_type) => {
                match self.property_of(&receiver_type, name) {
                    Some((location, ty)) => {
                        if !safe {
                            self.check_receiver_not_null(
                                receiver_node,
                                &receiver_type,
                                name_node,
                                location,
                            );
                        }
                        let root = self.receiver_root(receiver_node);
                        let ty = self.read_property(name_node, root, location, ty);
                        let ty = if safe { ty.nullable() } else { ty };
                        self.record(name_node, Reference::Declaration(location), ty.clone());
                        Typed::Value(ty)
                    }
                    None => {
                        // A function value's members (`::f.name`, `invoke`)
                        // aren't modelled.
                        let modelled = !receiver_type.is_unknown()
                            && !matches!(receiver_type, Ty::Function(_));
                        if !modelled {
                            return Typed::Value(Ty::Unknown);
                        }
                        Typed::Value(
                            self.record_unresolved(name_node, NameSite::Member(receiver_type)),
                        )
                    }
                }
            }
            Typed::Class(class) => self.static_member(class, name_node, name),
            Typed::Package(package) => self.package_member(package, name_node, name),
        }
    }

    /// `value::name` / `Class::name` as the grammar sometimes splits it (a
    /// navigation with a `::` suffix): the members named so.
    fn member_reference(&mut self, receiver: &Typed, name_node: SyntaxNode<'_>, name: Name) -> Ty {
        let receiver_type = match receiver {
            Typed::Value(ty) => ty.clone(),
            Typed::Class(class) => Ty::Class(self.types.self_type(*class)),
            Typed::Package(_) => return Ty::Unknown,
        };
        let members: Vec<ItemLocation> = self
            .types
            .members_named(&receiver_type, name)
            .into_iter()
            .map(|member| member.location)
            .collect();
        match &members[..] {
            [] => {
                if !receiver_type.is_unknown() && !matches!(receiver_type, Ty::Function(_)) {
                    self.record_unresolved(name_node, NameSite::Member(receiver_type));
                }
            }
            [only] => self.record(name_node, Reference::Declaration(*only), Ty::Unknown),
            several => self.record(
                name_node,
                Reference::Candidates(several.into()),
                Ty::Unknown,
            ),
        }
        Ty::Unknown
    }

    /// `Class.name`: a nested class, an enum entry, a Java static, or a
    /// member of the companion (or of the object itself).
    fn static_member(
        &mut self,
        class: ItemLocation,
        name_node: SyntaxNode<'_>,
        name: Name,
    ) -> Typed {
        let statics = self.types.static_members_named(class, name);
        if let Some(found) = statics.first() {
            let found = *found;
            if self.item_kind(found).is_classifier() {
                let ty = self.value_of(Typed::Class(found));
                self.record(name_node, Reference::Declaration(found), ty);
                return Typed::Class(found);
            }
            let ty = self.types.property_type(found);
            self.record(name_node, Reference::Declaration(found), ty.clone());
            return Typed::Value(ty);
        }
        if let Some(instance) = self.types.object_instance(class)
            && let Some((location, ty)) = self.property_of(&Ty::Class(instance), name)
        {
            self.record(name_node, Reference::Declaration(location), ty.clone());
            return Typed::Value(ty);
        }
        if let Some(entries) = self.enum_entries(class, name) {
            self.record(name_node, Reference::Declaration(class), entries.clone());
            return Typed::Value(entries);
        }
        Typed::Value(self.record_unresolved(name_node, NameSite::Static))
    }

    /// `entries` inside an enum (or its companion) means the enum's.
    fn enum_entries_in_scope(&self, name: Name) -> Option<(ItemLocation, Ty)> {
        if name.as_str() != "entries" {
            return None;
        }
        (0..self.receivers.len()).rev().find_map(|index| {
            let receiver_type = self.receiver_type(index);
            let class = receiver_type.class()?.class;
            let tree = hir_def::item_tree(self.db, class.container);
            let item = tree.item(class.item);
            let enum_class = match item.kind {
                ItemKind::Class(hir_def::item_tree::ClassKind::CompanionObject) => ItemLocation {
                    container: class.container,
                    item: item.parent?,
                },
                _ => class,
            };
            Some((enum_class, self.enum_entries(enum_class, name)?))
        })
    }

    /// `Enum.entries`.
    fn enum_entries(&self, class: ItemLocation, name: Name) -> Option<Ty> {
        let is_enum = self.item_kind(class) == ItemKind::Class(hir_def::item_tree::ClassKind::Enum);
        if !is_enum || name.as_str() != "entries" {
            return None;
        }
        let entry = Ty::Class(self.types.self_type(class));
        Some(crate::lower::builtin(
            self.db,
            self.file,
            crate::lower::KnownPackage::KotlinEnums,
            "EnumEntries",
            vec![entry],
        ))
    }

    /// `package.name`: a class or top-level property of the package, or a
    /// subpackage.
    fn package_member(&mut self, package: Path, name_node: SyntaxNode<'_>, name: Name) -> Typed {
        let found = hir_def::def_map::visible_top_level(self.db, self.file, &package, name);
        if let Some(class) = found
            .iter()
            .find(|location| self.item_kind(**location).is_classifier())
        {
            let ty = self.value_of(Typed::Class(*class));
            self.record(name_node, Reference::Declaration(*class), ty);
            return Typed::Class(*class);
        }
        if let Some(property) = found
            .iter()
            .find(|location| matches!(self.item_kind(**location), ItemKind::Property { .. }))
        {
            let ty = self.types.property_type(*property);
            self.record(name_node, Reference::Declaration(*property), ty.clone());
            return Typed::Value(ty);
        }
        let subpackage = package.join(name);
        if hir_def::def_map::package_prefix_exists(self.db, self.file, &subpackage) {
            return Typed::Package(subpackage);
        }
        Typed::Value(self.record_unresolved(name_node, NameSite::Package))
    }

    // --- simple expressions -------------------------------------------------

    fn string_template(&mut self, node: SyntaxNode<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "interpolated_expression" => {
                    for inner in named_children(child) {
                        self.expression(inner, None);
                    }
                }
                "interpolated_identifier" if self.text(child) != "this" => {
                    self.identifier(child);
                }
                _ => {}
            }
        }
    }

    fn this(&mut self, node: SyntaxNode<'_>) -> Ty {
        self.this_receiver(node)
            .map(|index| self.receiver_type(index))
            .unwrap_or(Ty::Unknown)
    }

    /// Which implicit receiver `this` (or `this@label`) is: its depth.
    pub(super) fn this_receiver(&self, node: SyntaxNode<'_>) -> Option<usize> {
        let label = named_children(node)
            .find(|child| child.kind() == "type_identifier" || child.kind() == "simple_identifier")
            .map(|label| self.name(label));
        match label {
            Some(label) => self
                .receivers
                .iter()
                .rposition(|receiver| receiver.label == Some(label)),
            None => self.receivers.len().checked_sub(1),
        }
    }

    /// `super` on its own: the supertype it names, when only one can be
    /// meant.
    fn super_type(&mut self, node: SyntaxNode<'_>) -> Ty {
        match &self.super_candidates(node)[..] {
            [only] => only.clone(),
            _ => Ty::Unknown,
        }
    }

    /// `super.member`: of the supertypes `super` may name, the one that has
    /// `member` (Kotlin looks the member up in all of them).
    pub(super) fn super_with_member(&mut self, node: SyntaxNode<'_>, member: Name) -> Ty {
        let candidates = self.super_candidates(node);
        let mut having = candidates
            .iter()
            .filter(|candidate| !self.types.members_named(candidate, member).is_empty());
        match (having.next(), having.next()) {
            (Some(only), None) => only.clone(),
            _ => Ty::Unknown,
        }
    }

    /// The direct supertypes `super` may mean here: of the innermost class
    /// body (or the one `super@Label` names), narrowed to `C` by
    /// `super<C>`.
    fn super_candidates(&self, node: SyntaxNode<'_>) -> Vec<Ty> {
        let label = named_children(node)
            .find(|child| child.kind() == "type_identifier" || child.kind() == "simple_identifier")
            .map(|label| self.name(label));
        let Some(body) = self.receivers.iter().rev().find(|receiver| {
            let has_super = receiver.super_of != SuperOf::Nothing;
            has_super && (label.is_none() || receiver.label == label)
        }) else {
            return Vec::new();
        };
        let candidates: Vec<Ty> = match body.super_of {
            SuperOf::Class(class) => self
                .types
                .direct_supertypes(&self.types.self_type(class))
                .into_iter()
                .map(Ty::Class)
                .collect(),
            SuperOf::ObjectLiteral => vec![body.ty.clone()],
            SuperOf::Nothing => Vec::new(),
        };
        let Some(qualifier) = named_children(node).find(|child| child.kind() == "user_type") else {
            return candidates;
        };
        let named = self.written_type(qualifier);
        let Some(named_class) = named.class().map(|class| class.class) else {
            return Vec::new();
        };
        candidates
            .into_iter()
            .filter(|candidate| {
                candidate
                    .class()
                    .is_some_and(|class| class.class == named_class)
            })
            .collect()
    }

    /// A member access's receiver: `super` is the supertype with the member.
    pub(super) fn member_receiver(
        &mut self,
        receiver: SyntaxNode<'_>,
        member: Option<Name>,
    ) -> Typed {
        match (receiver.kind(), member) {
            ("super_expression", Some(member)) => {
                Typed::Value(self.super_with_member(receiver, member))
            }
            _ => self.typed(receiver, None),
        }
    }

    fn prefix(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let Some(operand) = named_children(node).last() else {
            return Ty::Unknown;
        };
        if has_token(node, "!") {
            self.condition(node);
            return self.types.builtin("Boolean");
        }
        let operand_type = self.expression(operand, expected);
        if has_token(node, "-") {
            return self
                .operator_result(&operand_type, "unaryMinus", Vec::new())
                .unwrap_or(operand_type);
        }
        operand_type
    }

    fn postfix(&mut self, node: SyntaxNode<'_>) -> Ty {
        let Some(operand) = named_children(node).next() else {
            return Ty::Unknown;
        };
        let operand_type = self.expression(operand, None);
        if let Some(assertion) = syntax::token(node, "!!") {
            self.check_null_check(
                operand,
                &operand_type,
                NullCheckKind::NotNullAssertion,
                TextRange::of(assertion),
            );
            // `x!!` throws on null: from here on `x` isn't null.
            let not_null = self.not_null_chain(operand);
            self.narrow(&not_null);
            return operand_type.non_null();
        }
        operand_type
    }

    fn cast(&mut self, node: SyntaxNode<'_>) -> Ty {
        let mut children = named_children(node);
        let (Some(operand), Some(target)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let operand_type = self.expression(operand, None);
        self.examine_cast(node, operand, &operand_type, target);
        let target_type = self.written_type(target);
        if has_token(node, "as?") {
            return target_type.nullable();
        }
        // `x as T` throws unless `x` is a `T`: from here on it is one.
        if let Some(subject) = self.narrowable(operand) {
            let narrowed = self.smart_cast(&self.narrowed_type(subject), target_type.clone());
            self.narrow(&[(subject, narrowed)]);
        }
        target_type
    }

    /// `a + b`, `a * b`, `a..b`: the operator function of `a`'s type.
    fn binary_operator(&mut self, node: SyntaxNode<'_>) -> Ty {
        let mut children = named_children(node);
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let left_type = self.expression(left, None);
        let right_type = self.expression(right, None);
        match reassociation::operator_function(node) {
            Some(function) => self
                .operator_result(&left_type, function, vec![right_type])
                .unwrap_or(Ty::Unknown),
            None => Ty::Unknown,
        }
    }

    /// `a to b`, `0 until n`: an infix call.
    fn infix(&mut self, node: SyntaxNode<'_>) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let [left, function, right] = children[..] else {
            for child in children {
                self.expression(child, None);
            }
            return Ty::Unknown;
        };
        let receiver = self.expression(left, None);
        self.infix_call(receiver, function, right)
    }

    fn elvis(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Option<Requirement>,
    ) -> Ty {
        let mut children = named_children(node);
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let nullable_expected = expected.map(|expected| expected.clone().nullable());
        let left_type = self.expression(left, nullable_expected.as_ref());
        if let Some(operator) = syntax::token(node, "?:") {
            let removable = TextRange::new(left.end_byte(), node.end_byte());
            self.check_null_check(
                left,
                &left_type,
                NullCheckKind::Elvis { removable },
                TextRange::of(operator),
            );
        }
        // Two ways on: the left side wasn't null (nor what its safe chain
        // went through), or the right side ran — `x ?: return` leaves the
        // first only.
        let not_null = self.not_null_chain(left);
        let ((), left_way) = self.fork(|this| this.narrow(&not_null));
        let right_type = match requirement {
            Some(requirement) => self.required(right, expected, requirement),
            None => self.expression(right, expected),
        };
        self.end_way(&right_type);
        let right_way = std::mem::take(&mut self.flow);
        self.flow = self.merge(vec![left_way, right_way]);
        self.types.common_type(&[left_type.non_null(), right_type])
    }

    /// `receiver?.member`: records it when the receiver can't be null.
    pub(super) fn check_safe_call(
        &mut self,
        receiver_node: SyntaxNode<'_>,
        receiver: &Typed,
        suffix: SyntaxNode<'_>,
    ) {
        let (Typed::Value(receiver_type), Some(operator)) = (receiver, syntax::token(suffix, "?."))
        else {
            return;
        };
        self.check_null_check(
            receiver_node,
            receiver_type,
            NullCheckKind::SafeCall,
            TextRange::of(operator),
        );
    }

    fn indexing(&mut self, node: SyntaxNode<'_>) -> Ty {
        let mut children = named_children(node);
        let (Some(receiver), Some(suffix)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let receiver_type = self.expression(receiver, None);
        let indices: Vec<Ty> = named_children(suffix)
            .map(|index| self.expression(index, None))
            .collect();
        let element = self
            .operator_result(&receiver_type, "get", indices)
            .unwrap_or(Ty::Unknown);
        // `holder?.tags[0]` is in the safe call's chain: null when `holder`
        // is.
        if receiver_type.nullability() == Nullability::Nullable {
            element.nullable()
        } else {
            element
        }
    }

    fn assignment(&mut self, node: SyntaxNode<'_>) {
        let mut children = named_children(node);
        let (Some(target), Some(value)) = (children.next(), children.next()) else {
            return;
        };
        let target_type = self.assignable(target);
        let is_plain = has_token(node, "=");
        let expected = if is_plain { Some(&target_type) } else { None };
        let required = is_plain.then(|| self.written_target_type(target)).flatten();
        let ty = match required {
            Some(required) => {
                let requirement = super::type_mismatches::Requirement::new(
                    super::MismatchSite::Assignment,
                    &required,
                );
                self.required(value, expected, requirement)
            }
            None => self.expression(value, expected),
        };
        if is_plain {
            self.narrow_assigned(target, &ty);
        }
    }

    /// `x = value` makes a local `var` what was assigned: `var t: T? =
    /// null; t = load()` is a `T` after it. A value of a type that doesn't
    /// fit, or isn't known, leaves it its declared type.
    fn narrow_assigned(&mut self, target: SyntaxNode<'_>, assigned: &Ty) {
        let children: Vec<SyntaxNode<'_>> = named_children(target).collect();
        let [name] = children[..] else {
            return;
        };
        let Some(Reference::Local(local)) =
            self.recorded_at(name).map(|resolved| resolved.reference)
        else {
            return;
        };
        if self.local_kind(local) != LocalKind::Variable {
            return;
        }
        self.forget_aliased(local);
        self.assign_local(local, assigned);
    }

    /// The type an assignment's target writes: a local or property
    /// declared with one. One inferred from an initializer isn't judged
    /// (a branch's common type here may not be the compiler's), nor is a
    /// smart cast.
    fn written_target_type(&self, target: SyntaxNode<'_>) -> Option<Ty> {
        let children: Vec<SyntaxNode<'_>> = named_children(target).collect();
        let name = match children[..] {
            // An unqualified member of an unknown receiver may be meant.
            [only] if only.kind() == "simple_identifier" => {
                let ambiguous = !self.receivers_known() || self.in_uncertain_scope();
                if ambiguous
                    && matches!(
                        self.recorded_at(only).map(|resolved| resolved.reference),
                        Some(Reference::Declaration(_))
                    )
                {
                    return None;
                }
                only
            }
            [receiver, suffix] if suffix.kind() == "navigation_suffix" => {
                if !self.examinable(receiver) {
                    return None;
                }
                named_children(suffix).find(|child| child.kind() == "simple_identifier")?
            }
            _ => return None,
        };
        match self.recorded_at(name)?.reference {
            Reference::Local(local) => {
                let declared = self.local_declaration_writes_type(local);
                declared.then(|| self.local_declared_type(local))
            }
            Reference::Declaration(location) => {
                let tree = hir_def::item_tree(self.db, location.container);
                let written = matches!(
                    &tree.item(location.item).signature,
                    hir_def::item_tree::Signature::Property {
                        type_ref: Some(_),
                        ..
                    }
                );
                written.then(|| self.types.property_type(location))
            }
            _ => None,
        }
    }

    /// The left side of an assignment: a name, `receiver.name`, or
    /// `receiver[index]`.
    fn assignable(&mut self, node: SyntaxNode<'_>) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        match children[..] {
            [only] => self.expression(only, None),
            [receiver, suffix] if suffix.kind() == "navigation_suffix" => {
                let receiver_type = self.typed(receiver, None);
                let Some(name_node) =
                    named_children(suffix).find(|child| child.kind() == "simple_identifier")
                else {
                    return Ty::Unknown;
                };
                let name = self.name(name_node);
                match receiver_type {
                    Typed::Value(receiver_type) => match self.setter_target(&receiver_type, name) {
                        Some((location, ty)) => {
                            self.record(name_node, Reference::Declaration(location), ty.clone());
                            ty
                        }
                        None if receiver_type.is_unknown() => Ty::Unknown,
                        None => self.record_unresolved(name_node, NameSite::Member(receiver_type)),
                    },
                    Typed::Class(class) => {
                        let value = self.static_member(class, name_node, name);
                        self.value_of(value)
                    }
                    Typed::Package(_) => Ty::Unknown,
                }
            }
            [receiver, suffix] if suffix.kind() == "indexing_suffix" => {
                self.expression(receiver, None);
                for index in named_children(suffix) {
                    self.expression(index, None);
                }
                Ty::Unknown
            }
            _ => {
                for child in children {
                    self.expression(child, None);
                }
                Ty::Unknown
            }
        }
    }

    /// The property `receiver.name = …` sets — or the Java setter.
    fn setter_target(&mut self, receiver: &Ty, name: Name) -> Option<(ItemLocation, Ty)> {
        if let Some(found) = self.property_of(receiver, name) {
            return Some(found);
        }
        let spelled = name.as_str();
        let first = spelled.chars().next()?;
        let setter = Name::new(&format!(
            "set{}{}",
            first.to_uppercase(),
            &spelled[first.len_utf8()..]
        ));
        let member = self
            .types
            .members_named(receiver, setter)
            .into_iter()
            .find(|member| crate::members::is_java(self.db, member.location))?;
        let ty = self
            .types
            .signature(Callable::Function(member.location))
            .and_then(|signature| {
                signature
                    .parameters
                    .first()
                    .map(|parameter| parameter.ty.clone())
            })
            .map(|ty| ty.substitute(&member.substitution))
            .unwrap_or(Ty::Unknown);
        Some((member.location, ty))
    }

    // --- control flow ---------------------------------------------------

    fn if_expression(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Option<Requirement>,
    ) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&condition, branches)) = children.split_first() else {
            return Ty::Unknown;
        };
        let narrowing = self.condition(condition);
        let then_branch = branches.first().copied();
        let else_branch = branches.get(1).copied();
        let (then_type, then_end) = self.fork(|this| {
            this.narrow(&narrowing.when_true);
            let ty = match then_branch {
                Some(branch) => this.body_required(branch, expected, requirement.clone()),
                None => this.types.builtin("Unit"),
            };
            this.end_way(&ty);
            ty
        });
        self.narrow(&narrowing.when_false);
        let else_type =
            else_branch.map(|branch| self.body_required(branch, expected, requirement.clone()));
        if let Some(else_type) = &else_type {
            self.end_way(else_type);
        }
        // What follows knows what both ways it can come from know; a way
        // that jumps (`return`, `throw`) doesn't come.
        let else_end = std::mem::take(&mut self.flow);
        self.flow = self.merge(vec![then_end, else_end]);
        match else_type {
            Some(else_type) => self.types.common_type(&[then_type, else_type]),
            None => self.types.builtin("Unit"),
        }
    }

    fn when_expression(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Option<Requirement>,
    ) -> Ty {
        self.push_scope();
        let mut subject_local = None;
        let mut subject_type = None;
        let subject_value = named_children(node)
            .find(|child| child.kind() == "when_subject")
            .and_then(|subject| {
                named_children(subject)
                    .filter(|child| {
                        child.kind() != "variable_declaration"
                            && child.kind() != "binding_pattern_kind"
                    })
                    .last()
            });
        let mut subject_examinable = false;
        let mut not_null_when_matched = Vec::new();
        let mut branches = Vec::new();
        let mut branch_ends = Vec::new();
        let mut has_else = false;
        for child in named_children(node) {
            match child.kind() {
                "when_subject" => {
                    let (local, ty) = self.when_subject(child);
                    subject_local = local;
                    subject_type = Some(ty);
                    subject_examinable =
                        subject_value.is_some_and(|subject| self.examinable(subject));
                    not_null_when_matched = subject_value
                        .map(|subject| self.not_null_chain(subject))
                        .unwrap_or_default();
                }
                "when_entry" => {
                    let subject = WhenSubject {
                        local: subject_local,
                        ty: subject_type.as_ref(),
                        examinable: subject_examinable,
                        not_null_when_matched: &not_null_when_matched,
                    };
                    let branch = self.when_entry(child, subject, expected, requirement.clone());
                    has_else |= branch.is_else;
                    branches.push(branch.ty);
                    branch_ends.push(branch.end);
                    // Later branches run only when this one's conditions
                    // were false.
                    let mut when_false = self.narrowed_when_entry_false(child, subject);
                    when_false.extend(branch.when_false);
                    self.narrow(&when_false);
                }
                _ => {}
            }
        }
        // Without `else`, no branch may run — unless the branches cover
        // every case of the subject (an enum's entries, a sealed type's
        // subclasses), as the compiler sees it.
        let covered = !has_else
            && subject_type
                .as_ref()
                .is_some_and(|subject_type| self.covers_all_cases(node, subject_type));
        if !has_else && !covered {
            branch_ends.push(self.flow.clone());
        }
        self.pop_scope();
        // What follows knows what every way it can come from knows.
        self.flow = self.merge(branch_ends);
        if let Some(subject_type) = &subject_type {
            self.record_when_subject(node, subject_type);
        }
        let exhaustive_enough = has_else || expected.is_some();
        if branches.is_empty() || !exhaustive_enough && subject_type.is_none() {
            return self.types.builtin("Unit");
        }
        self.types.common_type(&branches)
    }

    /// `when (x)` or `when (val y = …)`: the local the subject is (to
    /// smart-cast in branches), and its type.
    fn when_subject(&mut self, node: SyntaxNode<'_>) -> (Option<Narrowed>, Ty) {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        if let Some(variable) = children
            .iter()
            .find(|child| child.kind() == "variable_declaration")
        {
            let value = children.iter().rev().find(|child| {
                child.kind() != "variable_declaration" && child.kind() != "binding_pattern_kind"
            });
            let ty = match value {
                Some(value) => self.expression(*value, None),
                None => Ty::Unknown,
            };
            let written = self
                .type_child(*variable)
                .map(|written| self.written_type(written));
            let ty = written.unwrap_or(ty);
            let local = named_children(*variable)
                .find(|part| part.kind() == "simple_identifier")
                .map(|name| Narrowed::Local(self.declare_node(name, ty.clone(), LocalKind::Value)));
            return (local, ty);
        }
        let Some(subject) = children.last() else {
            return (None, Ty::Unknown);
        };
        let ty = self.expression(*subject, None);
        (self.narrowable(*subject), ty)
    }

    /// One `when` branch: its conditions (with the smart casts they give
    /// the branch) and its body; whether it is `else`, and what is narrowed
    /// where it ends.
    fn when_entry(
        &mut self,
        node: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
        expected: Option<&Ty>,
        requirement: Option<super::type_mismatches::Requirement>,
    ) -> WhenBranch {
        let mut conditions: Vec<super::narrowing::Narrowing> = Vec::new();
        let mut body = None;
        for child in named_children(node) {
            match child.kind() {
                "when_condition" => {
                    conditions.push(self.when_condition(child, subject));
                }
                "control_structure_body" => body = Some(child),
                _ => {}
            }
        }
        let is_else = conditions.is_empty();
        // The branch runs when any condition holds: what they all say. The
        // branches after it see every condition false.
        let when_true = conditions
            .iter()
            .map(|condition| condition.when_true.clone())
            .reduce(|known, next| self.join_narrowings(&known, &next))
            .unwrap_or_default();
        let when_false: Vec<(Narrowed, Ty)> = conditions
            .iter()
            .flat_map(|condition| condition.when_false.iter().cloned())
            .collect();
        let (ty, end) = self.fork(|this| {
            this.push_scope();
            this.narrow(&when_true);
            let ty = match body {
                Some(body) => this.body_required(body, expected, requirement),
                None => this.types.builtin("Unit"),
            };
            this.pop_scope();
            this.end_way(&ty);
            ty
        });
        WhenBranch {
            ty,
            is_else,
            end,
            when_false,
        }
    }

    /// What a branch whose one condition is `!is T` tells the branches
    /// after it: the subject is a `T`.
    fn narrowed_when_entry_false(
        &self,
        entry: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
    ) -> Vec<(Narrowed, Ty)> {
        let conditions: Vec<SyntaxNode<'_>> = named_children(entry)
            .filter(|child| child.kind() == "when_condition")
            .collect();
        // `null ->` (without a guard): later branches see the subject
        // not null.
        let names_null = conditions.iter().any(|condition| {
            named_children(*condition)
                .next()
                .is_some_and(|value| value.kind() == "null_literal")
        });
        if names_null && !syntax::ast::kotlin::has_guard(entry) {
            let subject_local = subject
                .local
                .map(|local| (local, self.narrowed_type(local).non_null()));
            return subject_local
                .into_iter()
                .chain(subject.not_null_when_matched.iter().cloned())
                .collect();
        }
        let [condition] = conditions[..] else {
            return Vec::new();
        };
        let (Some(local), Some(test)) = (subject.local, named_children(condition).next()) else {
            return Vec::new();
        };
        let is_negated_type_test =
            test.kind() == "type_test" && TypeTest::of(test) == TypeTest::IsNot;
        let Some(target) = self.type_child(test).filter(|_| is_negated_type_test) else {
            return Vec::new();
        };
        let target = self.written_type(target);
        vec![(local, self.smart_cast(&self.narrowed_type(local), target))]
    }

    /// `is T` as a branch's condition on the subject. The last branch of
    /// an exhaustive `when` isn't examined: the compiler accepts it there.
    fn examine_subject_test(
        &mut self,
        test: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
        target: SyntaxNode<'_>,
        negated: bool,
    ) {
        let subject_type = match (subject.local, subject.ty) {
            (Some(local), _) => self.narrowed_type(local),
            (None, Some(ty)) => ty.clone(),
            (None, None) => Ty::Unknown,
        };
        let examined = subject.examinable && !is_last_of_several_branches(test);
        let subject_type = if examined { subject_type } else { Ty::Unknown };
        self.examine_type_test(test, None, &subject_type, target, negated);
    }

    fn when_condition(
        &mut self,
        node: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
    ) -> super::narrowing::Narrowing {
        let subject_type = subject.ty;
        let mut outcome = super::narrowing::Narrowing::default();
        for child in named_children(node) {
            match child.kind() {
                "type_test" => {
                    let Some(target) = self.type_child(child) else {
                        continue;
                    };
                    let target_type = self.written_type(target);
                    let negated = TypeTest::of(child) == TypeTest::IsNot;
                    self.examine_subject_test(child, subject, target, negated);
                    if negated {
                        continue;
                    }
                    if let Some(local) = subject.local {
                        let narrowed = self.smart_cast(&self.narrowed_type(local), target_type);
                        outcome.when_true.push((local, narrowed));
                    }
                }
                "range_test" => {
                    for operand in named_children(child) {
                        self.expression(operand, None);
                    }
                }
                _ => {
                    if subject_type.is_some() {
                        let matched = self.expression(child, subject_type);
                        if self.excludes_null(&matched) {
                            outcome
                                .when_true
                                .extend(subject.not_null_when_matched.iter().cloned());
                        }
                    } else {
                        // Without a subject a branch is a condition: the
                        // branches after it see it false.
                        let narrowing = self.condition(child);
                        outcome.when_true.extend(narrowing.when_true);
                        outcome.when_false.extend(narrowing.when_false);
                    }
                }
            }
        }
        outcome
    }

    fn try_expression(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        requirement: Option<Requirement>,
    ) -> Ty {
        let mut branches = Vec::new();
        // A `catch` may start anywhere in the try block: with what was
        // known before it, but of nothing the block assigns.
        let catch_start = {
            let mut start = self.flow.clone();
            let block =
                named_children(node).find(|child| matches!(child.kind(), "statements" | "block"));
            for variable in block
                .map(|block| self.reassigned_in(block))
                .unwrap_or_default()
            {
                start.forget(Narrowed::Local(variable));
            }
            start
        };
        let mut ends: Vec<Flow> = Vec::new();
        let mut finally = None;
        for child in named_children(node) {
            match child.kind() {
                "statements" | "block" => {
                    let ty = self.block_required(child, requirement.clone());
                    self.end_way(&ty);
                    branches.push(ty);
                    ends.push(self.flow.clone());
                }
                "catch_block" => {
                    self.flow = catch_start.clone();
                    let caught = self.catch_block(child, requirement.clone());
                    self.end_way(&caught);
                    ends.push(self.flow.clone());
                    branches.push(caught);
                }
                "finally_block" => finally = Some(child),
                _ => {
                    self.expression(child, expected);
                }
            }
        }
        // Past the `try`, one of its ways went on; then `finally` ran.
        self.flow = self.merge(ends);
        if let Some(finally) = finally {
            for part in named_children(finally) {
                self.block(part);
            }
        }
        if branches.is_empty() {
            return self.types.builtin("Unit");
        }
        self.types.common_type(&branches)
    }

    fn catch_block(
        &mut self,
        node: SyntaxNode<'_>,
        requirement: Option<super::type_mismatches::Requirement>,
    ) -> Ty {
        self.push_scope();
        let exception = self
            .type_child(node)
            .map(|written| self.written_type(written))
            .unwrap_or(Ty::Unknown);
        if let Some(name) = named_children(node).find(|child| child.kind() == "simple_identifier") {
            let local = self.declare_node(name, exception.clone(), LocalKind::Parameter);
            self.mark_certain_if_known(local, &exception);
        }
        let mut ty = self.types.builtin("Unit");
        for child in named_children(node) {
            if child.kind() == "statements" {
                ty = self.statements_required(child, requirement.clone());
            }
        }
        self.pop_scope();
        ty
    }

    fn for_statement(&mut self, node: SyntaxNode<'_>) {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let variable = children
            .iter()
            .find(|child| {
                matches!(
                    child.kind(),
                    "variable_declaration" | "multi_variable_declaration"
                )
            })
            .copied();
        let iterated = children
            .iter()
            .find(|child| {
                !matches!(
                    child.kind(),
                    "variable_declaration"
                        | "multi_variable_declaration"
                        | "control_structure_body"
                        | "annotation"
                        | "label"
                )
            })
            .copied();
        let iterated_type = match iterated {
            Some(iterated) => self.expression(iterated, None),
            None => Ty::Unknown,
        };
        let element = self.element_type(&iterated_type);
        let reassigned = self.reassigned_in(node);
        self.forget_reassigned(&reassigned);
        // The body may run no time, or many: past the loop, what the entry
        // and every way out of an iteration know.
        let entry = self.flow.clone();
        self.enter_loop(node);
        self.push_scope();
        match variable {
            Some(variable) if variable.kind() == "multi_variable_declaration" => {
                self.destructure(variable, &element, LocalKind::Value);
            }
            Some(variable) => {
                let written = self
                    .type_child(variable)
                    .map(|written| self.written_type(written));
                if let Some(name) =
                    named_children(variable).find(|part| part.kind() == "simple_identifier")
                {
                    self.declare_node(name, written.unwrap_or(element), LocalKind::Value);
                }
            }
            None => {}
        }
        if let Some(body) = children
            .iter()
            .find(|child| child.kind() == "control_structure_body")
        {
            self.body(*body, None);
        }
        self.pop_scope();
        let frame = self.leave_loop();
        let body_end = std::mem::take(&mut self.flow);
        let mut ways = vec![entry, body_end];
        ways.extend(frame.continues);
        ways.extend(frame.breaks);
        self.flow = self.merge(ways);
    }

    /// What iterating a value gives: `iterator().next()`.
    pub(super) fn element_type(&mut self, iterated: &Ty) -> Ty {
        let Some(iterator) = self.operator_result(iterated, "iterator", Vec::new()) else {
            return Ty::Unknown;
        };
        self.operator_result(&iterator, "next", Vec::new())
            .unwrap_or(Ty::Unknown)
    }

    /// `while (c) body`, `do body while (c)`. As the compiler's data flow
    /// does, what was known of a `var` the loop assigns is forgotten as it
    /// enters (a later iteration may come with another value); where the
    /// loop ends without `break`, its condition is false.
    fn while_statement(&mut self, node: SyntaxNode<'_>) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let body = children
            .iter()
            .find(|child| child.kind() == "control_structure_body")
            .copied();
        let condition = children
            .iter()
            .find(|child| child.kind() != "control_structure_body" && child.kind() != "label")
            .copied();
        let reassigned = self.reassigned_in(node);
        self.forget_reassigned(&reassigned);
        self.enter_loop(node);
        // `while (true)`: the condition is never false, the loop ends by a
        // `break` only.
        let endless = condition.is_some_and(|condition| {
            condition.kind() == "boolean_literal" && self.text(condition) == "true"
        });
        let by_condition = if node.kind() == "do_while_statement" {
            self.do_while(body, condition)
        } else {
            let narrowing = condition.map(|condition| self.condition(condition));
            // Each iteration starts as the first did: what the loop assigns
            // was forgotten on entering.
            let entry = self.flow.clone();
            if let Some(body) = body {
                self.push_scope();
                if let Some(narrowing) = &narrowing {
                    self.narrow(&narrowing.when_true);
                }
                self.body(body, None);
                self.pop_scope();
            }
            self.flow = entry;
            if let Some(narrowing) = narrowing {
                self.narrow(&narrowing.when_false);
            }
            std::mem::take(&mut self.flow)
        };
        let frame = self.leave_loop();
        let mut ways = frame.breaks;
        if !endless {
            ways.push(by_condition);
        }
        self.flow = self.merge(ways);
        if self.flow.is_dead() {
            Ty::Nothing { nullable: false }
        } else {
            self.types.builtin("Unit")
        }
    }

    /// `do body while (c)`: the body runs once at least, and the condition
    /// sees where it ended.
    fn do_while(
        &mut self,
        body: Option<SyntaxNode<'_>>,
        condition: Option<SyntaxNode<'_>>,
    ) -> Flow {
        self.push_scope();
        if let Some(body) = body {
            // The body's own locals are the condition's too.
            for statement in named_children(body) {
                self.statements(statement);
            }
        }
        // The condition is reached from the body's end and every `continue`.
        let continues =
            std::mem::take(&mut self.loops.last_mut().expect("inside the loop").continues);
        let mut reaching = vec![std::mem::take(&mut self.flow)];
        reaching.extend(continues);
        self.flow = self.merge(reaching);
        let narrowing = condition.map(|condition| self.condition(condition));
        self.pop_scope();
        if let Some(narrowing) = narrowing {
            self.narrow(&narrowing.when_false);
        }
        std::mem::take(&mut self.flow)
    }

    /// The `var`s `loop` assigns, as named where it starts.
    fn reassigned_in(&self, loop_node: SyntaxNode<'_>) -> Vec<super::LocalId> {
        let mut reassigned = Vec::new();
        for part in syntax::descendants(loop_node) {
            let target = match part.kind() {
                "assignment" => named_children(part)
                    .next()
                    .filter(|target| target.kind() == "directly_assignable_expression")
                    .and_then(|target| named_children(target).next()),
                "postfix_expression" | "prefix_expression"
                    if has_token(part, "++") || has_token(part, "--") =>
                {
                    named_children(part).find(|operand| operand.kind() == "simple_identifier")
                }
                _ => None,
            };
            let Some(name) = target.filter(|target| target.kind() == "simple_identifier") else {
                continue;
            };
            let variable = self
                .lookup_value_local(self.name(name))
                .filter(|local| self.local_kind(*local) == LocalKind::Variable);
            if let Some(variable) = variable
                && !reassigned.contains(&variable)
            {
                reassigned.push(variable);
            }
        }
        reassigned
    }

    /// Forgets what was known of `variables`: their declared types again.
    fn forget_reassigned(&mut self, variables: &[super::LocalId]) {
        for variable in variables {
            self.forget_aliased(*variable);
            self.forget(Narrowed::Local(*variable));
        }
    }

    // --- functions as values --------------------------------------------

    /// A lambda; `expected` gives its parameters' types and its receiver.
    pub(super) fn lambda(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        label: Option<Name>,
    ) -> Ty {
        let expected_function = match expected {
            Some(Ty::Function(function)) => Some((**function).clone()),
            Some(Ty::Class(class)) => self.types.functional_interface_type(class),
            _ => None,
        };
        // Without its function type the lambda may have a receiver whose
        // members its body uses: misses inside are uncertain.
        let shape_unknown = expected_function.is_none();
        if shape_unknown {
            self.enter_uncertain_scope();
        }
        // The body runs on its own copy of the flow: when, and how often,
        // is the called function's to say (see `narrow_after_contract`).
        let outer_flow = self.flow.clone();
        let outer_loops = std::mem::take(&mut self.loops);
        self.push_scope();
        let receiver = expected_function
            .as_ref()
            .and_then(|function| function.receiver.clone());
        if let Some(receiver) = &receiver {
            self.push_receiver(receiver.clone(), label);
        }
        let parameters = self.lambda_parameters(node, expected_function.as_ref());
        let expected_result = expected_function
            .as_ref()
            .map(|function| function.result.clone());
        let body = named_children(node).find(|child| child.kind() == "statements");
        let body_type = self.lambda_value(body, expected_result.as_ref());
        let body_jumps = matches!(body_type, Ty::Nothing { nullable: false });
        let returns_unit = expected_result
            .as_ref()
            .is_some_and(|result| self.is_unit(result));
        let result = if returns_unit {
            self.types.builtin("Unit")
        } else {
            body_type
        };
        if receiver.is_some() {
            self.pop_receiver();
        }
        if shape_unknown {
            self.leave_uncertain_scope();
        }
        self.pop_scope();
        let end = std::mem::replace(&mut self.flow, outer_flow);
        self.loops = outer_loops;
        self.leave_lambda_flow(node, body, end, body_jumps);
        Ty::Function(Box::new(FunctionType {
            receiver,
            parameters,
            result,
            suspend: expected_function
                .as_ref()
                .is_some_and(|function| function.suspend),
            nullability: Nullability::NotNull,
        }))
    }

    /// A lambda's parameters, declared in its scope: as written, typed as
    /// written or as `expected` gives them (`it` when it declares none and
    /// takes one).
    fn lambda_parameters(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&FunctionType>,
    ) -> Vec<Ty> {
        let expected_parameters: &[Ty] = expected
            .map(|function| function.parameters.as_slice())
            .unwrap_or_default();
        let declared = named_children(node).find(|child| child.kind() == "lambda_parameters");
        let Some(declared) = declared else {
            let takes_one = expected.is_none() || expected_parameters.len() == 1;
            if !takes_one {
                return Vec::new();
            }
            let ty = expected_parameters.first().cloned().unwrap_or(Ty::Unknown);
            let brace = TextRange::new(node.start_byte(), node.start_byte() + 1);
            self.declare(Name::new("it"), brace, ty.clone(), LocalKind::ImplicitIt);
            return vec![ty];
        };
        let mut parameters = Vec::new();
        for (index, parameter) in named_children(declared).enumerate() {
            let expected_parameter = expected_parameters
                .get(index)
                .cloned()
                .unwrap_or(Ty::Unknown);
            let written = self
                .type_child(parameter)
                .map(|written| self.written_type(written));
            let ty = written.unwrap_or(expected_parameter);
            self.declare_lambda_parameter(parameter, &ty);
            parameters.push(ty);
        }
        parameters
    }

    /// One written lambda parameter: a name (`_` declares nothing) or a
    /// destructuring `(key, value)`.
    fn declare_lambda_parameter(&mut self, parameter: SyntaxNode<'_>, ty: &Ty) {
        if parameter.kind() == "multi_variable_declaration" {
            self.destructure(parameter, ty, LocalKind::LambdaParameter);
            return;
        }
        if let Some(name) =
            named_children(parameter).find(|part| part.kind() == "simple_identifier")
            && self.text(name) != "_"
        {
            self.declare_node(name, ty.clone(), LocalKind::LambdaParameter);
        }
    }

    /// The value of a lambda's body (`Unit` when it has none), a `return`
    /// in it returning from the lambda.
    fn lambda_value(&mut self, body: Option<SyntaxNode<'_>>, expected_result: Option<&Ty>) -> Ty {
        let Some(body) = body else {
            return self.types.builtin("Unit");
        };
        self.return_targets
            .push(super::declarations::ReturnTarget::Lambda);
        let body_type = self.lambda_body(body, expected_result);
        self.return_targets.pop();
        body_type
    }

    /// The flow a lambda's body ended with, kept for the call that runs it
    /// (`narrow_after_contract`): dead when the body always jumps out of
    /// the function around (`apply { …; return x }`), not kept when a
    /// `return@label` leaves it before its end. A `var` the lambda assigns
    /// may change whenever it runs: nothing known of it holds past here.
    fn leave_lambda_flow(
        &mut self,
        node: SyntaxNode<'_>,
        body: Option<SyntaxNode<'_>>,
        mut end: Flow,
        body_jumps: bool,
    ) {
        if body_jumps {
            end.kill();
        }
        for variable in self.reassigned_in(node) {
            self.forget_aliased(variable);
            self.make_unsure(Narrowed::Local(variable));
        }
        let leaves_early = body.is_some_and(|body| self.has_labelled_return(body));
        if !leaves_early {
            self.lambda_ends.insert(node.start_byte() as u32, end);
        }
    }

    /// A lambda's statements; the last one's value is the lambda's — unless
    /// a labelled `return@…` gives it another, whose type isn't followed:
    /// then unknown.
    fn lambda_body(&mut self, node: SyntaxNode<'_>, expected_result: Option<&Ty>) -> Ty {
        let statements: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&last, leading)) = statements.split_last() else {
            return self.types.builtin("Unit");
        };
        for statement in leading {
            self.statement(*statement);
        }
        let last_type = self.last_statement(last, expected_result);
        if self.has_labelled_return(node) {
            return Ty::Unknown;
        }
        last_type
    }

    /// Whether a `return@label` is anywhere in `node`.
    fn has_labelled_return(&self, node: SyntaxNode<'_>) -> bool {
        syntax::descendants(node).any(|descendant| {
            descendant.kind() == "jump_expression" && self.text(descendant).starts_with("return@")
        })
    }

    fn last_statement(&mut self, last: SyntaxNode<'_>, expected_result: Option<&Ty>) -> Ty {
        match last.kind() {
            "property_declaration"
            | "function_declaration"
            | "class_declaration"
            | "assignment" => {
                self.statement(last);
                self.types.builtin("Unit")
            }
            _ => self.expression(last, expected_result),
        }
    }

    pub(super) fn is_unit(&self, ty: &Ty) -> bool {
        matches!((ty, self.types.builtin("Unit")), (Ty::Class(class), Ty::Class(unit)) if class.class == unit.class)
    }

    /// `return x`, `throw e`, `break`, `continue`: `x` is checked against
    /// what a plain `return` returns from, when that's a function writing
    /// its type.
    fn jump(&mut self, node: SyntaxNode<'_>) {
        let keyword = self.text(node);
        if keyword.starts_with("break") || keyword.starts_with("continue") {
            let label = named_children(node)
                .find(|child| child.kind() == "label")
                .map(|label| Name::new(self.text(label).trim_matches('@')));
            let way = self.flow.clone();
            if let Some(frame) = self.loop_frame(label) {
                if keyword.starts_with("break") {
                    frame.breaks.push(way);
                } else {
                    frame.continues.push(way);
                }
            }
            return;
        }
        let is_plain_return =
            has_token(node, "return") && !named_children(node).any(|child| child.kind() == "label");
        let expected = match self.return_targets.last() {
            Some(super::declarations::ReturnTarget::Function(Some(declared)))
                if is_plain_return =>
            {
                Some(declared.clone())
            }
            _ => None,
        };
        for child in named_children(node) {
            if child.kind() == "label" {
                continue;
            }
            // Inferred without the declared type as a hint: generic
            // inference here would bind from it alone (`fold(…)` to a
            // `Set` where Kotlin weighs the initial `mutableSetOf()` too).
            match &expected {
                Some(expected) => {
                    let requirement = super::type_mismatches::Requirement::new(
                        super::MismatchSite::Return,
                        expected,
                    );
                    self.required(child, None, requirement);
                }
                None => {
                    self.expression(child, None);
                }
            }
        }
    }

    fn anonymous_function(&mut self, node: SyntaxNode<'_>) -> Ty {
        let result = self.function(node);
        let parameters = named_children(node)
            .find(|child| child.kind() == "function_value_parameters")
            .map(|parameters| {
                named_children(parameters)
                    .filter(|child| child.kind() == "parameter")
                    .map(|parameter| {
                        self.type_child(parameter)
                            .map(|written| self.written_type(written))
                            .unwrap_or(Ty::Unknown)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ty::Function(Box::new(FunctionType {
            receiver: None,
            parameters,
            result,
            suspend: false,
            nullability: Nullability::NotNull,
        }))
    }

    /// `object : Base() { … }`: its body sees the object as `this`.
    fn object_literal(&mut self, node: SyntaxNode<'_>) -> Ty {
        let mut supertypes = Vec::new();
        for child in named_children(node) {
            if child.kind() == "delegation_specifier" {
                self.delegation(child);
                if let Some(written) =
                    syntax::descendants(child).find(|part| part.kind() == "user_type")
                {
                    supertypes.push(self.written_type(written));
                }
            }
        }
        let body = named_children(node).find(|child| child.kind() == "class_body");
        // The object's type is its supertype's only when it adds nothing to
        // it; its own members, or two supertypes at once, make a type that
        // isn't modelled: unknown rather than a type that lacks them.
        let adds_members = body.is_some_and(|body| self.declares_own_members(body));
        let ty = match &supertypes[..] {
            [only] if !adds_members => only.clone(),
            _ => Ty::Unknown,
        };
        if let Some(body) = body {
            self.push_body_receiver(ty.clone(), None, SuperOf::ObjectLiteral);
            for declaration in named_children(body) {
                self.declaration(declaration);
            }
            self.pop_receiver();
        }
        ty
    }

    /// Whether a class body declares a function or property that doesn't
    /// override one.
    fn declares_own_members(&self, body: SyntaxNode<'_>) -> bool {
        named_children(body)
            .filter(|member| {
                matches!(
                    member.kind(),
                    "function_declaration" | "property_declaration"
                )
            })
            .any(|member| {
                let overrides = named_children(member)
                    .filter(|part| part.kind() == "modifiers")
                    .any(|modifiers| {
                        syntax::descendants(modifiers).any(|part| self.text(part) == "override")
                    });
                !overrides
            })
    }
}

/// Whether `test` is a condition of the last of two or more branches of a
/// `when` without `else`.
fn is_last_of_several_branches(test: SyntaxNode<'_>) -> bool {
    let Some(entry) = test.parent().and_then(|condition| condition.parent()) else {
        return false;
    };
    let Some(when) = entry.parent() else {
        return false;
    };
    let entries: Vec<SyntaxNode<'_>> = named_children(when)
        .filter(|child| child.kind() == "when_entry")
        .collect();
    let has_else = entries
        .iter()
        .any(|entry| !named_children(*entry).any(|child| child.kind() == "when_condition"));
    entries.len() >= 2 && !has_else && entries.last() == Some(&entry)
}
