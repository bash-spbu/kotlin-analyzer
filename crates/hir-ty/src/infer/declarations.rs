//! The walk's state — scopes, implicit receivers, what's been found — and
//! the declarations and statements it walks through.

use std::rc::Rc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, kotlin_type_ref};
use hir_def::resolver::top_level_levels;
use hir_def::{ItemIndex, ItemLocation, file_item_tree};
use intern::Name;
use rustc_hash::FxHashMap;
use syntax::ast::AstNode;
use syntax::ast::kotlin::{FunctionDeclaration, PropertyDeclaration};
use syntax::{Parse, SyntaxNode, TextRange, has_token, named_children};

use super::{InferenceResult, Local, LocalId, LocalKind, Reference, ResolvedName};
use crate::lower::{TypeScope, lower_type};
use crate::members::Types;
use crate::ty::{FunctionType, Nullability, Ty};

/// How deep expressions nest before the walk gives up on them.
pub(super) const MAX_DEPTH: u32 = 160;

/// One level of local names, and the smart casts in force there.
#[derive(Default)]
struct Scope {
    names: Vec<(Name, LocalId)>,
    narrowed: Vec<(LocalId, Ty)>,
}

/// A value members can be used on without naming it: `this` of a class or
/// of a lambda with a receiver.
#[derive(Clone)]
pub(super) struct ImplicitReceiver {
    pub(super) ty: Ty,
    /// What `this@label` calls it: the class's or the function's name.
    pub(super) label: Option<Name>,
}

pub(crate) struct Inferrer<'db> {
    pub(super) types: Types<'db>,
    pub(super) db: &'db dyn SourceDatabase,
    pub(super) file: SourceFile,
    pub(super) parse: &'db Parse,
    items_by_range: FxHashMap<TextRange, ItemIndex>,
    scopes: Vec<Scope>,
    pub(super) receivers: Vec<ImplicitReceiver>,
    result: InferenceResult,
    top_level: FxHashMap<Name, Rc<Vec<Vec<ItemLocation>>>>,
    pub(super) depth: u32,
}

impl<'db> Inferrer<'db> {
    pub(crate) fn new(
        db: &'db dyn SourceDatabase,
        file: SourceFile,
        parse: &'db Parse,
    ) -> Inferrer<'db> {
        let tree = file_item_tree(db, file);
        let items_by_range = tree
            .indexed_items()
            .map(|(index, item)| (item.range, index))
            .collect();
        Inferrer {
            types: Types::new(db, file),
            db,
            file,
            parse,
            items_by_range,
            scopes: vec![Scope::default()],
            receivers: Vec::new(),
            result: InferenceResult::default(),
            top_level: FxHashMap::default(),
            depth: 0,
        }
    }

    pub(crate) fn finish(self) -> InferenceResult {
        self.result
    }

    // --- names and text -------------------------------------------------

    pub(super) fn text(&self, node: SyntaxNode<'_>) -> &'db str {
        &self.parse.text()[node.byte_range()]
    }

    pub(super) fn name(&self, node: SyntaxNode<'_>) -> Name {
        Name::new(self.text(node).trim_matches('`'))
    }

    /// The workspace declaration a node declares.
    pub(super) fn item_of(&self, node: SyntaxNode<'_>) -> Option<ItemLocation> {
        self.items_by_range
            .get(&TextRange::of(node))
            .map(|index| ItemLocation::in_file(self.file, *index))
    }

    pub(super) fn item_kind(&self, location: ItemLocation) -> ItemKind {
        hir_def::item_tree(self.db, location.container)
            .item(location.item)
            .kind
    }

    /// The top-level declarations named `name` this file sees, by level.
    pub(super) fn top_level(&mut self, name: Name) -> Rc<Vec<Vec<ItemLocation>>> {
        if let Some(levels) = self.top_level.get(&name) {
            return levels.clone();
        }
        let levels = Rc::new(top_level_levels(self.db, self.file, name));
        self.top_level.insert(name, levels.clone());
        levels
    }

    /// A type written in this file.
    pub(super) fn written_type(&self, node: SyntaxNode<'_>) -> Ty {
        if node.kind() == "type_identifier" {
            // A bare class name, as `Type::class` and `Type::member` write it.
            let path = intern::Path::new([self.name(node)]);
            return match hir_def::resolver::resolve_type_path(
                self.db,
                self.file,
                &path,
                node.start_byte(),
            ) {
                hir_def::resolver::Resolution::Declaration(class) => {
                    let mut class_type = self.types.self_type(class);
                    class_type.arguments =
                        class_type.arguments.iter().map(|_| Ty::Unknown).collect();
                    Ty::Class(class_type)
                }
                _ => Ty::Unknown,
            };
        }
        let type_ref = kotlin_type_ref(self.parse, node);
        lower_type(
            self.db,
            TypeScope::written_in(self.db, self.file),
            &type_ref,
        )
    }

    /// The first type node among `node`'s children.
    pub(super) fn type_child<'tree>(&self, node: SyntaxNode<'tree>) -> Option<SyntaxNode<'tree>> {
        named_children(node).find(|child| syntax::ast::kotlin::is_type(*child))
    }

    // --- recording ------------------------------------------------------

    pub(super) fn record(&mut self, name: SyntaxNode<'_>, reference: Reference, ty: Ty) {
        self.result
            .names
            .insert(name.start_byte() as u32, ResolvedName { reference, ty });
    }

    pub(super) fn record_unresolved(&mut self, name: SyntaxNode<'_>) {
        let spelled = self.name(name);
        self.result
            .unresolved
            .push((name.start_byte() as u32, spelled));
    }

    // --- scopes ---------------------------------------------------------

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
        if self.scopes.is_empty() {
            self.scopes.push(Scope::default());
        }
    }

    pub(super) fn declare(
        &mut self,
        name: Name,
        name_range: TextRange,
        ty: Ty,
        kind: LocalKind,
    ) -> LocalId {
        let id = LocalId(self.result.locals.len() as u32);
        self.result.locals.push(Local {
            name,
            name_range,
            ty,
            kind,
        });
        self.result.local_declarations.insert(name_range.start, id);
        self.scopes
            .last_mut()
            .expect("there's always a scope")
            .names
            .push((name, id));
        id
    }

    /// Declares the local a `simple_identifier` node names.
    pub(super) fn declare_node(
        &mut self,
        name: SyntaxNode<'_>,
        ty: Ty,
        kind: LocalKind,
    ) -> LocalId {
        let spelled = self.name(name);
        self.declare(spelled, TextRange::of(name), ty, kind)
    }

    pub(super) fn lookup_local(&self, name: Name) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.names.iter().rev())
            .find(|(declared, _)| *declared == name)
            .map(|(_, id)| *id)
    }

    /// A local's type here: its declared or inferred type, or what a smart
    /// cast in force narrowed it to.
    pub(super) fn local_type(&self, local: LocalId) -> Ty {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.narrowed.iter().rev())
            .find(|(narrowed, _)| *narrowed == local)
            .map(|(_, ty)| ty.clone())
            .unwrap_or_else(|| self.result.locals[local.0 as usize].ty.clone())
    }

    pub(super) fn narrow(&mut self, narrowings: &[(LocalId, Ty)]) {
        let scope = self.scopes.last_mut().expect("there's always a scope");
        scope.narrowed.extend(narrowings.iter().cloned());
    }

    pub(super) fn push_receiver(&mut self, ty: Ty, label: Option<Name>) {
        self.receivers.push(ImplicitReceiver { ty, label });
    }

    pub(super) fn pop_receiver(&mut self) {
        self.receivers.pop();
    }

    // --- declarations ---------------------------------------------------

    /// Walks every declaration of the file.
    pub(crate) fn file_declarations(&mut self) {
        let root = self.parse.root();
        for child in named_children(root) {
            self.declaration(child);
        }
    }

    /// The type of the declaration spanning `range`, inferred from its body.
    pub(crate) fn declaration_type(&mut self, range: TextRange) -> Ty {
        let root = self.parse.root();
        let Some(node) = root
            .descendant_for_byte_range(range.start as usize, range.end as usize)
            .and_then(|found| ancestor_spanning(found, range))
        else {
            return Ty::Unknown;
        };
        self.enter_enclosing_classes(node);
        match node.kind() {
            "function_declaration" => self.function(node),
            "property_declaration" => self.member_property(node),
            _ => Ty::Unknown,
        }
    }

    /// Pushes the receivers of the classes around `node`, outermost first.
    fn enter_enclosing_classes(&mut self, node: SyntaxNode<'_>) {
        let mut classes = Vec::new();
        let mut current = node.parent();
        while let Some(ancestor) = current {
            if matches!(
                ancestor.kind(),
                "class_declaration" | "object_declaration" | "companion_object"
            ) {
                classes.push(ancestor);
            }
            current = ancestor.parent();
        }
        for class in classes.into_iter().rev() {
            if let Some(location) = self.item_of(class) {
                self.push_class_receivers(location);
            }
        }
    }

    pub(super) fn declaration(&mut self, node: SyntaxNode<'_>) {
        match node.kind() {
            "class_declaration" | "object_declaration" | "companion_object" => self.class(node),
            "function_declaration" => {
                self.function(node);
            }
            "property_declaration" => {
                self.member_property(node);
            }
            "secondary_constructor" => self.secondary_constructor(node),
            "anonymous_initializer" => {
                for child in named_children(node) {
                    self.block(child);
                }
            }
            _ => {}
        }
    }

    /// `this` of the class, and of its companion object below it.
    /// `this` of the class, below it its companion object, and below that
    /// the companions of its superclasses (whose members a subclass uses
    /// unqualified too).
    fn push_class_receivers(&mut self, class: ItemLocation) -> usize {
        let mut pushed = 0;
        let mut companions: Vec<ItemLocation> = self.types.companion(class).into_iter().collect();
        for supertype in self
            .types
            .all_supertypes(&self.types.self_type(class))
            .iter()
            .skip(1)
        {
            companions.extend(self.types.companion(supertype.class));
        }
        for companion in companions.into_iter().rev() {
            let ty = Ty::Class(self.types.self_type(companion));
            self.push_receiver(ty, None);
            pushed += 1;
        }
        let tree = hir_def::item_tree(self.db, class.container);
        let label = tree.item(class.item).name;
        let ty = Ty::Class(self.types.self_type(class));
        self.push_receiver(ty, Some(label));
        pushed + 1
    }

    fn class(&mut self, node: SyntaxNode<'_>) {
        let pushed = match self.item_of(node) {
            Some(class) => self.push_class_receivers(class),
            None => 0,
        };
        self.push_scope();
        for child in named_children(node) {
            match child.kind() {
                "primary_constructor" => self.constructor_parameters(child),
                "delegation_specifier" | "delegation_specifiers" => self.delegation(child),
                "class_body" | "enum_class_body" => self.class_body(child),
                _ => {}
            }
        }
        self.pop_scope();
        for _ in 0..pushed {
            self.pop_receiver();
        }
    }

    fn constructor_parameters(&mut self, node: SyntaxNode<'_>) {
        for parameter in named_children(node).filter(|child| child.kind() == "class_parameter") {
            let ty = self
                .type_child(parameter)
                .map(|written| self.written_type(written))
                .unwrap_or(Ty::Unknown);
            let default = named_children(parameter)
                .skip_while(|child| !syntax::ast::kotlin::is_type(*child))
                .nth(1);
            if let Some(default) = default {
                self.expression(default, Some(&ty));
            }
            if let Some(name) =
                named_children(parameter).find(|child| child.kind() == "simple_identifier")
            {
                self.declare_node(name, ty, LocalKind::Parameter);
            }
        }
    }

    /// A supertype list: constructor calls' arguments and `by` delegates.
    pub(super) fn delegation(&mut self, node: SyntaxNode<'_>) {
        for descendant in named_children(node) {
            match descendant.kind() {
                "delegation_specifier" => self.delegation(descendant),
                "constructor_invocation" => {
                    for arguments in
                        named_children(descendant).filter(|child| child.kind() == "value_arguments")
                    {
                        self.walk_arguments(arguments);
                    }
                }
                "explicit_delegation" => {
                    if let Some(delegate) = named_children(descendant).last() {
                        self.expression(delegate, None);
                    }
                }
                _ => {}
            }
        }
    }

    fn class_body(&mut self, node: SyntaxNode<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "enum_entry" => self.enum_entry(child),
                _ => self.declaration(child),
            }
        }
    }

    fn enum_entry(&mut self, node: SyntaxNode<'_>) {
        for child in named_children(node) {
            match child.kind() {
                "value_arguments" => self.walk_arguments(child),
                "class_body" => self.class_body(child),
                _ => {}
            }
        }
    }

    fn secondary_constructor(&mut self, node: SyntaxNode<'_>) {
        self.push_scope();
        for child in named_children(node) {
            match child.kind() {
                "function_value_parameters" => self.parameters(child),
                "constructor_delegation_call" => {
                    for arguments in
                        named_children(child).filter(|part| part.kind() == "value_arguments")
                    {
                        self.walk_arguments(arguments);
                    }
                }
                "statements" | "block" => {
                    self.block(child);
                }
                _ => {}
            }
        }
        self.pop_scope();
    }

    /// A function's parameters, declared as locals; their default values
    /// inferred.
    fn parameters(&mut self, node: SyntaxNode<'_>) {
        let mut vararg = false;
        let mut last_type = Ty::Unknown;
        for child in named_children(node) {
            match child.kind() {
                "parameter_modifiers" => {
                    vararg = named_children(child).any(|modifier| has_token(modifier, "vararg"));
                }
                "parameter" => {
                    let written = self
                        .type_child(child)
                        .map(|written| self.written_type(written))
                        .unwrap_or(Ty::Unknown);
                    let ty = if vararg {
                        crate::lower::builtin(
                            self.db,
                            self.file,
                            "kotlin",
                            "Array",
                            vec![written.clone()],
                        )
                    } else {
                        written.clone()
                    };
                    if let Some(name) =
                        named_children(child).find(|part| part.kind() == "simple_identifier")
                    {
                        self.declare_node(name, ty.clone(), LocalKind::Parameter);
                    }
                    last_type = written;
                    vararg = false;
                }
                _ => {
                    let expected = last_type.clone();
                    self.expression(child, Some(&expected));
                }
            }
        }
    }

    /// A function, local or not; returns its result type (written, or its
    /// expression body's).
    pub(super) fn function(&mut self, node: SyntaxNode<'_>) -> Ty {
        // An anonymous function has the same parts.
        let declaration = FunctionDeclaration::new_unchecked(node);
        let label = declaration.name().map(|name| self.name(name));
        let receiver_type = declaration
            .receiver()
            .map(|receiver| self.written_type(receiver));
        let return_type = declaration
            .return_type()
            .map(|written| self.written_type(written));
        let pushed_receiver = receiver_type.is_some();
        if let Some(receiver) = receiver_type {
            self.push_receiver(receiver, label);
        }
        self.push_scope();
        for child in named_children(node) {
            if child.kind() == "function_value_parameters" {
                self.parameters(child);
            }
        }
        let body = named_children(node).find(|child| child.kind() == "function_body");
        let result = match body {
            Some(body) => self.function_body(body, return_type.as_ref()),
            None => return_type.clone().unwrap_or(Ty::Unknown),
        };
        self.pop_scope();
        if pushed_receiver {
            self.pop_receiver();
        }
        return_type.unwrap_or(result)
    }

    /// `= expression` gives its type; a block gives `Unit`.
    fn function_body(&mut self, body: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let is_block = has_token(body, "{");
        let content = named_children(body).next();
        if is_block {
            if let Some(statements) = content {
                self.block(statements);
            }
            return self.types.builtin("Unit");
        }
        match content {
            Some(expression) => self.expression(expression, expected),
            None => Ty::Unknown,
        }
    }

    /// A property declared in a class or file: its type, written or
    /// inferred from what initializes it.
    fn member_property(&mut self, node: SyntaxNode<'_>) -> Ty {
        let (receiver_type, written) = self.property_types(node);
        let pushed_receiver = receiver_type.is_some();
        if let Some(receiver) = receiver_type {
            self.push_receiver(receiver, None);
        }
        let initialized = self.property_initializer(node, written.as_ref());
        for child in named_children(node) {
            match child.kind() {
                "getter" => {
                    if let Some(body) =
                        named_children(child).find(|part| part.kind() == "function_body")
                    {
                        let ty = self.function_body(body, written.as_ref());
                        if written.is_none() && initialized.is_none() && has_token(body, "=") {
                            if pushed_receiver {
                                self.pop_receiver();
                            }
                            return ty;
                        }
                    }
                }
                "setter" => self.setter(child, written.clone().or(initialized.clone())),
                _ => {}
            }
        }
        if pushed_receiver {
            self.pop_receiver();
        }
        written.or(initialized).unwrap_or(Ty::Unknown)
    }

    fn setter(&mut self, node: SyntaxNode<'_>, property_type: Option<Ty>) {
        self.push_scope();
        for child in named_children(node) {
            match child.kind() {
                "parameter_with_optional_type" | "simple_identifier" => {
                    let name = if child.kind() == "simple_identifier" {
                        Some(child)
                    } else {
                        named_children(child).find(|part| part.kind() == "simple_identifier")
                    };
                    if let Some(name) = name {
                        let ty = property_type.clone().unwrap_or(Ty::Unknown);
                        self.declare_node(name, ty, LocalKind::Parameter);
                    }
                }
                "function_body" => {
                    self.function_body(child, None);
                }
                _ => {}
            }
        }
        self.pop_scope();
    }

    /// The extension receiver and the written type of a property.
    fn property_types(&self, node: SyntaxNode<'_>) -> (Option<Ty>, Option<Ty>) {
        let receiver = PropertyDeclaration::new_unchecked(node)
            .receiver()
            .map(|receiver| self.written_type(receiver));
        let written = named_children(node)
            .find(|child| child.kind() == "variable_declaration")
            .and_then(|variable| self.type_child(variable))
            .map(|written| self.written_type(written));
        (receiver, written)
    }

    /// The type an initializer (`= value`) or delegate (`by lazy { … }`)
    /// gives a property.
    fn property_initializer(&mut self, node: SyntaxNode<'_>, written: Option<&Ty>) -> Option<Ty> {
        let mut after_variable = false;
        for child in named_children(node) {
            match child.kind() {
                "variable_declaration" | "multi_variable_declaration" => after_variable = true,
                "property_delegate" => {
                    let delegate = named_children(child).last()?;
                    let delegate_type = self.expression(delegate, None);
                    return Some(self.delegated_type(&delegate_type));
                }
                "getter" | "setter" | "modifiers" | "type_constraints" | "binding_pattern_kind" => {
                }
                _ if after_variable => return Some(self.expression(child, written)),
                _ => {}
            }
        }
        None
    }

    /// The value type of a delegated property: what its delegate's
    /// `getValue` returns (`Lazy<T>` → `T`).
    fn delegated_type(&mut self, delegate: &Ty) -> Ty {
        self.operator_result(
            delegate,
            "getValue",
            vec![Ty::Nothing { nullable: true }, Ty::Unknown],
        )
        .unwrap_or(Ty::Unknown)
    }

    // --- statements -----------------------------------------------------

    /// A block of statements in a new scope; its value is the last
    /// statement's.
    pub(super) fn block(&mut self, node: SyntaxNode<'_>) -> Ty {
        self.push_scope();
        let ty = self.statements(node);
        self.pop_scope();
        ty
    }

    /// Statements in the current scope.
    pub(super) fn statements(&mut self, node: SyntaxNode<'_>) -> Ty {
        if node.kind() != "statements" {
            return self.statement(node);
        }
        let mut last = self.types.builtin("Unit");
        for statement in named_children(node) {
            last = self.statement(statement);
        }
        last
    }

    pub(super) fn statement(&mut self, node: SyntaxNode<'_>) -> Ty {
        match node.kind() {
            "property_declaration" => {
                self.local_property(node);
                self.types.builtin("Unit")
            }
            "function_declaration" => {
                self.local_function(node);
                self.types.builtin("Unit")
            }
            "class_declaration" | "object_declaration" => {
                self.class(node);
                self.types.builtin("Unit")
            }
            "label" | "line_comment" | "multiline_comment" => self.types.builtin("Unit"),
            "statements" => self.block(node),
            _ => self.expression(node, None),
        }
    }

    fn local_property(&mut self, node: SyntaxNode<'_>) {
        let (_, written) = self.property_types(node);
        let initialized = self.property_initializer(node, written.as_ref());
        let mutable = named_children(node)
            .find(|child| child.kind() == "binding_pattern_kind")
            .is_some_and(|kind| has_token(kind, "var"));
        let kind = if mutable {
            LocalKind::Variable
        } else {
            LocalKind::Value
        };
        let value_type = written.clone().or(initialized).unwrap_or(Ty::Unknown);
        for child in named_children(node) {
            match child.kind() {
                "variable_declaration" => {
                    if let Some(name) =
                        named_children(child).find(|part| part.kind() == "simple_identifier")
                    {
                        self.declare_node(name, value_type.clone(), kind);
                    }
                }
                "multi_variable_declaration" => self.destructure(child, &value_type, kind),
                _ => {}
            }
        }
    }

    /// `(a, b)` of a value: each gets its `componentN()`.
    pub(super) fn destructure(&mut self, node: SyntaxNode<'_>, value: &Ty, kind: LocalKind) {
        let variables: Vec<SyntaxNode<'_>> = named_children(node)
            .filter(|child| child.kind() == "variable_declaration")
            .collect();
        for (index, variable) in variables.into_iter().enumerate() {
            let written = self
                .type_child(variable)
                .map(|written| self.written_type(written));
            let ty = written.unwrap_or_else(|| {
                self.operator_result(value, &format!("component{}", index + 1), Vec::new())
                    .unwrap_or(Ty::Unknown)
            });
            if let Some(name) =
                named_children(variable).find(|part| part.kind() == "simple_identifier")
                && self.text(name) != "_"
            {
                self.declare_node(name, ty, kind);
            }
        }
    }

    fn local_function(&mut self, node: SyntaxNode<'_>) {
        let Some(name) = named_children(node).find(|child| child.kind() == "simple_identifier")
        else {
            return;
        };
        // Declared first: a local function may call itself.
        let parameters: Vec<Ty> = named_children(node)
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
        let declaration = FunctionDeclaration::new_unchecked(node);
        let written_result = declaration
            .return_type()
            .map(|written| self.written_type(written));
        let receiver = declaration
            .receiver()
            .map(|receiver| self.written_type(receiver));
        let has_block_body = named_children(node)
            .find(|child| child.kind() == "function_body")
            .is_some_and(|body| has_token(body, "{"));
        let declared_result = match (&written_result, has_block_body) {
            (Some(written), _) => written.clone(),
            (None, true) => self.types.builtin("Unit"),
            (None, false) => Ty::Unknown,
        };
        let id = self.declare_node(
            name,
            Ty::Function(Box::new(FunctionType {
                receiver,
                parameters,
                result: declared_result,
                suspend: false,
                nullability: Nullability::NotNull,
            })),
            LocalKind::Function,
        );
        let result = self.function(node);
        if written_result.is_none()
            && !has_block_body
            && let Ty::Function(function) = &mut self.result.locals[id.0 as usize].ty
        {
            function.result = result;
        }
    }

    /// Walks a `value_arguments` node whose call isn't resolved.
    pub(super) fn walk_arguments(&mut self, node: SyntaxNode<'_>) {
        for argument in named_children(node) {
            if let Some(value) = named_children(argument).last() {
                self.expression(value, None);
            }
        }
    }
}

/// The node at or above `node` that spans exactly `range`.
fn ancestor_spanning(node: SyntaxNode<'_>, range: TextRange) -> Option<SyntaxNode<'_>> {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if TextRange::of(candidate) == range {
            return Some(candidate);
        }
        if (candidate.end_byte() - candidate.start_byte()) > (range.end - range.start) as usize {
            return None;
        }
        current = candidate.parent();
    }
    None
}
