//! Expressions: what each evaluates to, and which declaration each name in
//! it means.

use hir_def::ItemLocation;
use hir_def::item_tree::{ItemKind, Signature};
use hir_def::resolver::{Resolution, resolve_type_path};
use intern::{Name, Path};
use syntax::ast::kotlin::TypeTest;
use syntax::{SyntaxNode, TextRange, has_token, named_children};

use super::declarations::{Inferrer, SuperOf};
use super::{LocalId, LocalKind, NameSite, Reference};
use crate::signature::Callable;
use crate::ty::{FunctionType, Nullability, Ty};

/// What a `when` branch's conditions test.
#[derive(Clone, Copy)]
struct WhenSubject<'subject> {
    /// The local the subject is (`when (x)`), to smart-cast.
    local: Option<super::LocalId>,
    ty: Option<&'subject Ty>,
    /// `when (this)`: an `is T` branch sees `this` as a `T`.
    is_this: bool,
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
        if self.depth >= super::declarations::MAX_DEPTH {
            return Typed::Value(Ty::Unknown);
        }
        self.depth += 1;
        let typed = self.typed_inner(node, expected);
        self.depth -= 1;
        typed
    }

    fn typed_inner(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Typed {
        let value = match node.kind() {
            "simple_identifier" => return self.identifier(node),
            "navigation_expression" => return self.navigation(node),
            "parenthesized_expression" => match named_children(node).next() {
                Some(inner) => return self.typed(inner, expected),
                None => Ty::Unknown,
            },
            "call_expression" => self.call(node, expected),
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
            "elvis_expression" => self.elvis(node, expected),
            "indexing_expression" => self.indexing(node),
            "if_expression" => self.if_expression(node, expected),
            "when_expression" => self.when_expression(node, expected),
            "try_expression" => self.try_expression(node, expected),
            "jump_expression" => {
                for child in named_children(node) {
                    if child.kind() != "label" {
                        self.expression(child, None);
                    }
                }
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
            "while_statement" | "do_while_statement" => {
                self.while_statement(node);
                self.types.builtin("Unit")
            }
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
        if let Some(local) = self.lookup_local(name) {
            let ty = self.local_type(local);
            self.record(node, Reference::Local(local), ty.clone());
            return Typed::Value(ty);
        }
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receivers[index].ty.clone();
            if let Some((location, ty)) = self.property_of(&receiver, name) {
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
            self.record(node, Reference::Declaration(location), ty.clone());
            return Typed::Value(ty);
        }
        let path = Path::new([name]);
        match resolve_type_path(self.db, self.file, &path, node.start_byte()) {
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
        self.record_unresolved(node, NameSite::Scope);
        Typed::Value(Ty::Unknown)
    }

    /// The functions named `name` an unqualified call could reach: members
    /// of the implicit receivers, then top-level ones.
    fn functions_named(&mut self, name: Name) -> Vec<ItemLocation> {
        let mut found: Vec<ItemLocation> = Vec::new();
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receivers[index].ty.clone();
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
        if let Some(member) = members
            .iter()
            .find(|member| matches!(self.item_kind(member.location), ItemKind::Property { .. }))
        {
            let ty = self
                .types
                .property_type(member.location)
                .substitute(&member.substitution);
            return Some((member.location, ty));
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
        let name_node = named_children(suffix).find(|child| child.kind() == "simple_identifier");
        let member = name_node.map(|name| self.name(name));
        let receiver = self.member_receiver(receiver_node, member);
        let safe = has_token(suffix, "?.");
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
                        let ty = if safe { ty.nullable() } else { ty };
                        self.record(name_node, Reference::Declaration(location), ty.clone());
                        Typed::Value(ty)
                    }
                    None => {
                        // A function value's members (`::f.name`, `invoke`)
                        // aren't modelled.
                        let modelled = !receiver_type.is_unknown()
                            && !matches!(receiver_type, Ty::Function(_));
                        if modelled {
                            self.record_unresolved(
                                name_node,
                                NameSite::Member(receiver_type.clone()),
                            );
                        }
                        Typed::Value(Ty::Unknown)
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
        self.record_unresolved(name_node, NameSite::Static);
        Typed::Value(Ty::Unknown)
    }

    /// `entries` inside an enum (or its companion) means the enum's.
    fn enum_entries_in_scope(&self, name: Name) -> Option<(ItemLocation, Ty)> {
        if name.as_str() != "entries" {
            return None;
        }
        self.receivers.iter().rev().find_map(|receiver| {
            let class = receiver.ty.class()?.class;
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
            "kotlin.enums",
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
        self.record_unresolved(name_node, NameSite::Package);
        Typed::Value(Ty::Unknown)
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
        let label = named_children(node)
            .find(|child| child.kind() == "type_identifier" || child.kind() == "simple_identifier")
            .map(|label| self.name(label));
        let receiver = match label {
            Some(label) => self
                .receivers
                .iter()
                .rev()
                .find(|receiver| receiver.label == Some(label)),
            None => self.receivers.last(),
        };
        receiver
            .map(|receiver| receiver.ty.clone())
            .unwrap_or(Ty::Unknown)
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
        if has_token(node, "!!") {
            // `x!!` throws on null: from here on `x` isn't null.
            if let Some(narrowing) = self.asserted_not_null(operand) {
                self.narrow(&[narrowing]);
            }
            return operand_type.non_null();
        }
        operand_type
    }

    fn cast(&mut self, node: SyntaxNode<'_>) -> Ty {
        let mut children = named_children(node);
        let (Some(operand), Some(target)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        self.expression(operand, None);
        let target_type = self.written_type(target);
        if has_token(node, "as?") {
            return target_type.nullable();
        }
        // `x as T` throws unless `x` is a `T`: from here on it is one.
        let local = (operand.kind() == "simple_identifier")
            .then(|| self.lookup_local(self.name(operand)))
            .flatten();
        if let Some(local) = local {
            let narrowed = self.smart_cast(&self.local_type(local), target_type.clone());
            self.narrow(&[(local, narrowed)]);
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
        let operator = [
            ("+", "plus"),
            ("-", "minus"),
            ("*", "times"),
            ("/", "div"),
            ("%", "rem"),
            ("..<", "rangeUntil"),
            ("..", "rangeTo"),
        ]
        .into_iter()
        .find(|(token, _)| has_token(node, token))
        .map(|(_, function)| function);
        match operator {
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

    fn elvis(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let mut children = named_children(node);
        let (Some(left), Some(right)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let nullable_expected = expected.map(|expected| expected.clone().nullable());
        let left_type = self.expression(left, nullable_expected.as_ref());
        let right_type = self.expression(right, expected);
        let right_jumps = matches!(right_type, Ty::Nothing { nullable: false });
        if right_jumps && left.kind() == "simple_identifier" {
            // `x ?: return`: from here on `x` isn't null.
            if let Some(local) = self.lookup_local(self.name(left)) {
                let narrowed = self.local_type(local).non_null();
                self.narrow(&[(local, narrowed)]);
            }
        }
        self.types.common_type(&[left_type.non_null(), right_type])
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
        self.operator_result(&receiver_type, "get", indices)
            .unwrap_or(Ty::Unknown)
    }

    fn assignment(&mut self, node: SyntaxNode<'_>) {
        let mut children = named_children(node);
        let (Some(target), Some(value)) = (children.next(), children.next()) else {
            return;
        };
        let target_type = self.assignable(target);
        let is_plain = has_token(node, "=");
        let expected = if is_plain { Some(&target_type) } else { None };
        self.expression(value, expected);
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
                        None => {
                            if !receiver_type.is_unknown() {
                                self.record_unresolved(
                                    name_node,
                                    NameSite::Member(receiver_type.clone()),
                                );
                            }
                            Ty::Unknown
                        }
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

    fn if_expression(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&condition, branches)) = children.split_first() else {
            return Ty::Unknown;
        };
        let narrowing = self.condition(condition);
        let then_branch = branches.first().copied();
        let else_branch = branches.get(1).copied();
        self.push_scope();
        self.narrow(&narrowing.when_true);
        let receiver_narrowed = narrowing.receiver_when_true.clone();
        if let Some(receiver) = &receiver_narrowed {
            self.push_receiver(receiver.clone(), None);
        }
        let then_type = match then_branch {
            Some(branch) => self.body(branch, expected),
            None => self.types.builtin("Unit"),
        };
        if receiver_narrowed.is_some() {
            self.pop_receiver();
        }
        self.pop_scope();
        self.push_scope();
        self.narrow(&narrowing.when_false);
        let else_type = else_branch.map(|branch| self.body(branch, expected));
        self.pop_scope();
        let then_jumps = matches!(then_type, Ty::Nothing { nullable: false });
        if else_branch.is_none() && then_jumps {
            // `if (x == null) return`: what follows sees `x` as not null.
            self.narrow(&narrowing.when_false);
        }
        if else_type
            .as_ref()
            .is_some_and(|ty| matches!(ty, Ty::Nothing { nullable: false }))
        {
            self.narrow(&narrowing.when_true);
        }
        match else_type {
            Some(else_type) => self.types.common_type(&[then_type, else_type]),
            None => self.types.builtin("Unit"),
        }
    }

    fn when_expression(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        self.push_scope();
        let mut subject_local = None;
        let mut subject_type = None;
        let subject_is_this = named_children(node)
            .find(|child| child.kind() == "when_subject")
            .and_then(|subject| named_children(subject).last())
            .is_some_and(|subject| subject.kind() == "this_expression");
        let mut branches = Vec::new();
        let mut has_else = false;
        for child in named_children(node) {
            match child.kind() {
                "when_subject" => {
                    let (local, ty) = self.when_subject(child);
                    subject_local = local;
                    subject_type = Some(ty);
                }
                "when_entry" => {
                    let subject = WhenSubject {
                        local: subject_local,
                        ty: subject_type.as_ref(),
                        is_this: subject_is_this,
                    };
                    let (ty, is_else) = self.when_entry(child, subject, expected);
                    has_else |= is_else;
                    branches.push(ty);
                    // Later branches run only when this one's condition
                    // was false.
                    let when_false = self.narrowed_when_entry_false(child, subject);
                    self.narrow(&when_false);
                }
                _ => {}
            }
        }
        self.pop_scope();
        if let (false, Some(subject_type)) = (has_else, &subject_type) {
            self.check_exhaustive(node, subject_type);
        }
        let exhaustive_enough = has_else || expected.is_some();
        if branches.is_empty() || !exhaustive_enough && subject_type.is_none() {
            return self.types.builtin("Unit");
        }
        self.types.common_type(&branches)
    }

    /// `when (x)` or `when (val y = …)`: the local the subject is (to
    /// smart-cast in branches), and its type.
    fn when_subject(&mut self, node: SyntaxNode<'_>) -> (Option<super::LocalId>, Ty) {
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
                .map(|name| self.declare_node(name, ty.clone(), LocalKind::Value));
            return (local, ty);
        }
        let Some(subject) = children.last() else {
            return (None, Ty::Unknown);
        };
        let ty = self.expression(*subject, None);
        let local = if subject.kind() == "simple_identifier" {
            self.lookup_local(self.name(*subject))
        } else {
            None
        };
        (local, ty)
    }

    /// One `when` branch: its conditions (with the smart casts they give
    /// the branch) and its body.
    fn when_entry(
        &mut self,
        node: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
        expected: Option<&Ty>,
    ) -> (Ty, bool) {
        let mut each_condition = Vec::new();
        let mut receivers = Vec::new();
        let mut body = None;
        for child in named_children(node) {
            match child.kind() {
                "when_condition" => {
                    each_condition.push(self.when_condition(child, subject, &mut receivers));
                }
                "control_structure_body" => body = Some(child),
                _ => {}
            }
        }
        let is_else = each_condition.is_empty();
        self.push_scope();
        self.narrow(&narrowed_by_every(each_condition));
        // A branch with one `is T` of `this` sees `this` as a `T`.
        let narrowed_receiver = match &receivers[..] {
            [only] => Some(only.clone()),
            _ => None,
        };
        if let Some(receiver) = &narrowed_receiver {
            self.push_receiver(receiver.clone(), None);
        }
        let ty = match body {
            Some(body) => self.body(body, expected),
            None => self.types.builtin("Unit"),
        };
        if narrowed_receiver.is_some() {
            self.pop_receiver();
        }
        self.pop_scope();
        (ty, is_else)
    }

    /// What a branch whose one condition is `!is T` tells the branches
    /// after it: the subject is a `T`.
    fn narrowed_when_entry_false(
        &self,
        entry: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
    ) -> Vec<(LocalId, Ty)> {
        let conditions: Vec<SyntaxNode<'_>> = named_children(entry)
            .filter(|child| child.kind() == "when_condition")
            .collect();
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
        vec![(local, self.smart_cast(&self.local_type(local), target))]
    }

    fn when_condition(
        &mut self,
        node: SyntaxNode<'_>,
        subject: WhenSubject<'_>,
        receivers: &mut Vec<Ty>,
    ) -> Vec<(super::LocalId, Ty)> {
        let subject_type = subject.ty;
        let mut narrowings = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "type_test" => {
                    let Some(target) = self.type_child(child) else {
                        continue;
                    };
                    let target_type = self.written_type(target);
                    let negated = TypeTest::of(child) == TypeTest::IsNot;
                    if negated {
                        continue;
                    }
                    if let Some(local) = subject.local {
                        let narrowed = self.smart_cast(&self.local_type(local), target_type);
                        narrowings.push((local, narrowed));
                    } else if subject.is_this {
                        receivers.push(target_type);
                    }
                }
                "range_test" => {
                    for operand in named_children(child) {
                        self.expression(operand, None);
                    }
                }
                _ => {
                    if subject_type.is_some() {
                        self.expression(child, subject_type);
                    } else {
                        let narrowing = self.condition(child);
                        narrowings.extend(narrowing.when_true);
                    }
                }
            }
        }
        narrowings
    }

    fn try_expression(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let mut branches = Vec::new();
        for child in named_children(node) {
            match child.kind() {
                "statements" | "block" => branches.push(self.block(child)),
                "catch_block" => branches.push(self.catch_block(child)),
                "finally_block" => {
                    for part in named_children(child) {
                        self.block(part);
                    }
                }
                _ => {
                    self.expression(child, expected);
                }
            }
        }
        if branches.is_empty() {
            return self.types.builtin("Unit");
        }
        self.types.common_type(&branches)
    }

    fn catch_block(&mut self, node: SyntaxNode<'_>) -> Ty {
        self.push_scope();
        let exception = self
            .type_child(node)
            .map(|written| self.written_type(written))
            .unwrap_or(Ty::Unknown);
        if let Some(name) = named_children(node).find(|child| child.kind() == "simple_identifier") {
            self.declare_node(name, exception, LocalKind::Parameter);
        }
        let mut ty = self.types.builtin("Unit");
        for child in named_children(node) {
            if child.kind() == "statements" {
                ty = self.statements(child);
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
    }

    /// What iterating a value gives: `iterator().next()`.
    pub(super) fn element_type(&mut self, iterated: &Ty) -> Ty {
        let Some(iterator) = self.operator_result(iterated, "iterator", Vec::new()) else {
            return Ty::Unknown;
        };
        self.operator_result(&iterator, "next", Vec::new())
            .unwrap_or(Ty::Unknown)
    }

    fn while_statement(&mut self, node: SyntaxNode<'_>) {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let body = children
            .iter()
            .find(|child| child.kind() == "control_structure_body")
            .copied();
        let condition = children
            .iter()
            .find(|child| child.kind() != "control_structure_body" && child.kind() != "label")
            .copied();
        let narrowing = condition.map(|condition| self.condition(condition));
        if let Some(body) = body {
            self.push_scope();
            if let Some(narrowing) = &narrowing
                && node.kind() == "while_statement"
            {
                self.narrow(&narrowing.when_true);
            }
            self.body(body, None);
            self.pop_scope();
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
        self.push_scope();
        let receiver = expected_function
            .as_ref()
            .and_then(|function| function.receiver.clone());
        if let Some(receiver) = &receiver {
            self.push_receiver(receiver.clone(), label);
        }
        let expected_parameters: Vec<Ty> = expected_function
            .as_ref()
            .map(|function| function.parameters.clone())
            .unwrap_or_default();
        let declared = named_children(node).find(|child| child.kind() == "lambda_parameters");
        let mut parameters = Vec::new();
        match declared {
            Some(declared) => {
                for (index, parameter) in named_children(declared).enumerate() {
                    let expected_parameter = expected_parameters
                        .get(index)
                        .cloned()
                        .unwrap_or(Ty::Unknown);
                    let written = self
                        .type_child(parameter)
                        .map(|written| self.written_type(written));
                    let ty = written.unwrap_or(expected_parameter);
                    match parameter.kind() {
                        "multi_variable_declaration" => {
                            self.destructure(parameter, &ty, LocalKind::LambdaParameter);
                        }
                        _ => {
                            if let Some(name) = named_children(parameter)
                                .find(|part| part.kind() == "simple_identifier")
                                && self.text(name) != "_"
                            {
                                self.declare_node(name, ty.clone(), LocalKind::LambdaParameter);
                            }
                        }
                    }
                    parameters.push(ty);
                }
            }
            None => {
                let takes_one = expected_function.is_none() || expected_parameters.len() == 1;
                if takes_one {
                    let ty = expected_parameters.first().cloned().unwrap_or(Ty::Unknown);
                    let brace = TextRange::new(node.start_byte(), node.start_byte() + 1);
                    self.declare(Name::new("it"), brace, ty.clone(), LocalKind::ImplicitIt);
                    parameters.push(ty);
                }
            }
        }
        let expected_result = expected_function
            .as_ref()
            .map(|function| function.result.clone());
        let body = named_children(node).find(|child| child.kind() == "statements");
        let body_type = match body {
            Some(body) => self.lambda_body(body, expected_result.as_ref()),
            None => self.types.builtin("Unit"),
        };
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

    /// A lambda's statements; the last one's value is the lambda's.
    fn lambda_body(&mut self, node: SyntaxNode<'_>, expected_result: Option<&Ty>) -> Ty {
        let statements: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some((&last, leading)) = statements.split_last() else {
            return self.types.builtin("Unit");
        };
        for statement in leading {
            self.statement(*statement);
        }
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

    fn is_unit(&self, ty: &Ty) -> bool {
        matches!((ty, self.types.builtin("Unit")), (Ty::Class(class), Ty::Class(unit)) if class.class == unit.class)
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

/// A branch's conditions are alternatives (`is A, is B ->`): a local is
/// narrowed in the branch only to what every one of them narrows it to.
fn narrowed_by_every(mut each_condition: Vec<Vec<(LocalId, Ty)>>) -> Vec<(LocalId, Ty)> {
    let Some(first) = each_condition.pop() else {
        return Vec::new();
    };
    first
        .into_iter()
        .filter(|narrowing| each_condition.iter().all(|other| other.contains(narrowing)))
        .collect()
}
