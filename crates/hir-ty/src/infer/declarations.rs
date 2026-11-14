//! The walk's state — scopes, implicit receivers, what's been found — and
//! the declarations and statements it walks through.

use std::rc::Rc;

use base_db::{SourceDatabase, SourceFile};
use hir_def::item_tree::{ItemKind, kotlin_type_ref};
use hir_def::resolver::top_level_levels;
use hir_def::{ItemId, ItemLocation, file_item_tree};
use intern::Name;
use rustc_hash::FxHashMap;
use syntax::ast::AstNode;
use syntax::ast::kotlin::{FunctionDeclaration, PropertyDeclaration};
use syntax::{Parse, SyntaxNode, TextRange, has_token, named_children};

use super::{
    ArgumentMismatch, Certainty, InferenceResult, Local, LocalId, LocalKind, NameSite,
    NonExhaustiveWhen, Reference, ResolvedName, UnresolvedName, UnsafeCall,
};
use crate::lower::{TypeScope, lower_type};
use crate::members::Types;
use crate::ty::{FunctionType, Nullability, Ty};

/// How deep expressions nest before the walk gives up on them.
pub(super) const MAX_DEPTH: u32 = 160;

/// Names compiler plugins generate, which the model doesn't know: a miss
/// on one says nothing.
const PLUGIN_GENERATED_NAMES: &[&str] = &["serializer"];

/// One level of local names, and the smart casts in force there.
#[derive(Default)]
struct Scope {
    kind: ScopeKind,
    names: Vec<(Name, LocalId)>,
    narrowed: Vec<(LocalId, Ty)>,
}

/// What a scope belongs to, where that decides what's visible through it.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(super) enum ScopeKind {
    #[default]
    Block,
    /// A class's primary constructor parameters: seen by its initializers,
    /// not by its functions.
    ConstructorParameters,
    /// A function's (or accessor's) own scope.
    Function,
}

/// A value members can be used on without naming it: `this` of a class or
/// of a lambda with a receiver.
#[derive(Clone)]
pub(super) struct ImplicitReceiver {
    pub(super) ty: Ty,
    /// What `this@label` calls it: the class's or the function's name.
    pub(super) label: Option<Name>,
    pub(super) super_of: SuperOf,
    /// How many local scopes were open when it came into scope: locals of
    /// those are outside it, and its members hide them.
    pub(super) scope_depth: usize,
}

/// What `super` means where a receiver is in scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SuperOf {
    /// A class body: the class's direct supertypes.
    Class(ItemLocation),
    /// An object literal's body: its supertype, which is its receiver's
    /// type.
    ObjectLiteral,
    /// No `super` of its own: an extension's or a lambda's receiver, a
    /// companion's, a smart-cast `this`.
    Nothing,
}

pub(crate) struct Inferrer<'db> {
    pub(super) types: Types<'db>,
    pub(super) db: &'db dyn SourceDatabase,
    pub(super) file: SourceFile,
    pub(super) parse: &'db Parse,
    items_by_range: FxHashMap<TextRange, ItemId>,
    scopes: Vec<Scope>,
    pub(super) receivers: Vec<ImplicitReceiver>,
    result: InferenceResult,
    top_level: FxHashMap<Name, Rc<Vec<Vec<ItemLocation>>>>,
    pub(super) depth: u32,
    /// How many scopes of unknown shape enclose the walk.
    uncertain_scopes: u32,
    /// For completion: the position whose scope to capture.
    pub(super) probe: Option<super::completion::Probe>,
}

impl<'db> Inferrer<'db> {
    pub(crate) fn new(
        db: &'db dyn SourceDatabase,
        file: SourceFile,
        parse: &'db Parse,
    ) -> Inferrer<'db> {
        let tree = file_item_tree(db, file);
        let source_map = hir_def::file_item_source_map(db, file);
        let items_by_range = tree
            .indexed_items()
            .map(|(item, _)| (source_map.ranges(item).range, item))
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
            // A script's implicit receivers (Gradle's DSL) aren't modelled.
            uncertain_scopes: u32::from(is_script(parse)),
            probe: None,
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
        let range = TextRange {
            start: self.original_offset(node.start_byte() as u32),
            end: self.original_offset(node.end_byte() as u32),
        };
        self.items_by_range
            .get(&range)
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
            TypeScope::written_at(self.db, self.file, node.start_byte()),
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

    /// What was recorded for the name `node`, if anything.
    pub(super) fn recorded_at(&self, node: SyntaxNode<'_>) -> Option<ResolvedName> {
        self.result.names.get(&(node.start_byte() as u32)).cloned()
    }

    pub(super) fn record_unresolved(&mut self, name: SyntaxNode<'_>, site: NameSite) {
        let spelled = self.name(name);
        let certainty = self.lookup_certainty(spelled, &site);
        self.result.unresolved.push(UnresolvedName {
            range: TextRange::of(name),
            name: spelled,
            site,
            certainty,
        });
    }

    pub(super) fn record_argument_mismatch(&mut self, mismatch: ArgumentMismatch) {
        self.result.argument_mismatches.push(mismatch);
    }

    pub(super) fn record_unsafe_call(&mut self, call: UnsafeCall) {
        self.result.unsafe_calls.push(call);
    }

    pub(super) fn record_non_exhaustive_when(&mut self, when: NonExhaustiveWhen) {
        self.result.non_exhaustive_whens.push(when);
    }

    /// Whether a lookup of `name` at `site`, here, searched everything there
    /// is: no scope of unknown shape around, every receiver's class
    /// hierarchy resolved, and not a name compiler plugins generate.
    pub(super) fn lookup_certainty(&self, name: Name, site: &NameSite) -> Certainty {
        let generated_by_plugin = PLUGIN_GENERATED_NAMES.contains(&name.as_str());
        if self.uncertain_scopes > 0 || generated_by_plugin {
            return Certainty::Uncertain;
        }
        let receivers_known = self
            .receivers
            .iter()
            .all(|receiver| self.is_fully_known(&receiver.ty));
        let site_known = match site {
            NameSite::Member(receiver) => self.is_fully_known(receiver),
            NameSite::Scope | NameSite::Static | NameSite::Package => true,
        };
        if receivers_known && site_known {
            Certainty::Certain
        } else {
            Certainty::Uncertain
        }
    }

    /// A type whose members are all known: not unknown, and every
    /// supertype in its hierarchy resolves (a type parameter's: its
    /// bounds').
    fn is_fully_known(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Unknown => false,
            Ty::Class(class) => self.types.hierarchy_complete(class.class),
            // A bound that is another type parameter is judged where that
            // one is used (bounds may refer to each other in a cycle).
            Ty::TypeParameter { parameter, .. } => self
                .types
                .type_parameter_bounds(*parameter)
                .iter()
                .all(|bound| match bound {
                    Ty::TypeParameter { .. } => true,
                    other => self.is_fully_known(other),
                }),
            Ty::Function(_) | Ty::Nothing { .. } => true,
        }
    }

    /// Enters a scope whose shape isn't known (a lambda that may have a
    /// receiver); names missed inside it are uncertain.
    pub(super) fn enter_uncertain_scope(&mut self) {
        self.uncertain_scopes += 1;
    }

    pub(super) fn leave_uncertain_scope(&mut self) {
        self.uncertain_scopes -= 1;
    }

    // --- scopes ---------------------------------------------------------

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub(super) fn push_scope_of(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope {
            kind,
            ..Scope::default()
        });
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

    /// The local `name` means here — unless an implicit receiver that came
    /// into scope inside the local's scope has a member of the name, which
    /// Kotlin looks at first (`channel` in `produce { channel.send(x) }` is
    /// the `ProducerScope`'s, not a constructor parameter's).
    pub(super) fn lookup_local(&self, name: Name) -> Option<LocalId> {
        let (local, depth) = self.innermost_local(name)?;
        let inner_receivers: Vec<Ty> = self
            .receivers
            .iter()
            // A lambda's receiver comes right after the lambda's scope (its
            // parameters are as near as it is), a class's or an extension's
            // right before theirs: only a receiver of a scope nested in the
            // local's hides it.
            .filter(|receiver| receiver.scope_depth > depth + 1)
            .map(|receiver| receiver.ty.clone())
            .collect();
        let hidden = inner_receivers
            .iter()
            .any(|receiver| !self.types.members_named(receiver, name).is_empty());
        if hidden { None } else { Some(local) }
    }

    /// The innermost local named `name`, and the depth of its scope.
    fn innermost_local(&self, name: Name) -> Option<(LocalId, usize)> {
        let mut inside_function = false;
        for (depth, scope) in self.scopes.iter().enumerate().rev() {
            let hidden = inside_function && scope.kind == ScopeKind::ConstructorParameters;
            if !hidden
                && let Some((_, id)) = scope
                    .names
                    .iter()
                    .rev()
                    .find(|(declared, _)| *declared == name)
            {
                return Some((*id, depth));
            }
            inside_function |= scope.kind == ScopeKind::Function;
        }
        None
    }

    pub(super) fn local_declaration(&self, local: LocalId) -> &Local {
        self.result.local(local)
    }

    /// Every local visible here, innermost first; a shadowed one left out.
    pub(super) fn visible_locals(&self) -> Vec<LocalId> {
        let mut visible: Vec<LocalId> = Vec::new();
        let mut seen: Vec<Name> = Vec::new();
        let mut inside_function = false;
        for scope in self.scopes.iter().rev() {
            let hidden = inside_function && scope.kind == ScopeKind::ConstructorParameters;
            if !hidden {
                for (name, id) in scope.names.iter().rev() {
                    if !seen.contains(name) {
                        seen.push(*name);
                        visible.push(*id);
                    }
                }
            }
            inside_function |= scope.kind == ScopeKind::Function;
        }
        visible
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
        self.receivers.push(ImplicitReceiver {
            ty,
            label,
            super_of: SuperOf::Nothing,
            scope_depth: self.scopes.len(),
        });
    }

    /// A class body's (or an object literal's) `this`, which `super` refers
    /// past.
    pub(super) fn push_body_receiver(&mut self, ty: Ty, label: Option<Name>, super_of: SuperOf) {
        self.receivers.push(ImplicitReceiver {
            ty,
            label,
            super_of,
            scope_depth: self.scopes.len(),
        });
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
        self.annotations();
    }

    /// Every annotation's arguments: `name =` labels mean the annotation's
    /// members, values are expressions. What's in scope there (a class's
    /// constants for its members' annotations) isn't tracked, so misses
    /// are uncertain.
    fn annotations(&mut self) {
        let annotations: Vec<SyntaxNode<'_>> = syntax::descendants(self.parse.root())
            .filter(|node| node.kind() == "annotation")
            .collect();
        self.enter_uncertain_scope();
        for annotation in annotations {
            let Some(invocation) =
                named_children(annotation).find(|child| child.kind() == "constructor_invocation")
            else {
                continue;
            };
            let class = named_children(invocation)
                .find(|child| child.kind() == "user_type")
                .and_then(|written| self.written_type(written).class().map(|class| class.class));
            for arguments in
                named_children(invocation).filter(|child| child.kind() == "value_arguments")
            {
                for argument in
                    named_children(arguments).filter(|child| child.kind() == "value_argument")
                {
                    let children: Vec<SyntaxNode<'_>> = named_children(argument).collect();
                    let is_named = has_token(argument, "=") && children.len() >= 2;
                    if let (true, Some(class)) = (is_named, class) {
                        self.annotation_label(class, children[0]);
                    }
                    if let Some(value) = children.last() {
                        self.expression(*value, None);
                    }
                }
            }
        }
        self.leave_uncertain_scope();
    }

    /// `name` of `@Annotation(name = …)`: the annotation's member (a Java
    /// annotation's method, a Kotlin one's constructor property).
    fn annotation_label(&mut self, class: ItemLocation, label: SyntaxNode<'_>) {
        let name = self.name(label);
        let tree = hir_def::item_tree(self.db, class.container);
        let member = tree
            .item(class.item)
            .children
            .iter()
            .find(|child| {
                let child = tree.item(**child);
                child.name == name
                    && matches!(child.kind, ItemKind::Function | ItemKind::Property { .. })
            })
            .map(|child| ItemLocation {
                container: class.container,
                item: *child,
            });
        if let Some(member) = member {
            self.record(label, Reference::Declaration(member), Ty::Unknown);
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
            "function_declaration" => match function_result(node) {
                FunctionResult::Written(written) => self.written_type(written),
                FunctionResult::Block => self.types.builtin("Unit"),
                FunctionResult::Missing => Ty::Unknown,
                FunctionResult::Expression => self.function(node),
            },
            "property_declaration" => self.member_property(node),
            _ => Ty::Unknown,
        }
    }

    /// Pushes the receivers of the classes around `node`, outermost first.
    pub(super) fn enter_enclosing_classes(&mut self, node: SyntaxNode<'_>) {
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
            // Its initializers see its constructor's parameters.
            self.push_scope_of(ScopeKind::ConstructorParameters);
            let constructor =
                named_children(class).find(|child| child.kind() == "primary_constructor");
            for parameter in constructor.into_iter().flat_map(|constructor| {
                named_children(constructor).filter(|child| child.kind() == "class_parameter")
            }) {
                let ty = self
                    .type_child(parameter)
                    .map(|written| self.written_type(written))
                    .unwrap_or(Ty::Unknown);
                if let Some(name) =
                    named_children(parameter).find(|child| child.kind() == "simple_identifier")
                {
                    self.declare_node(name, ty, LocalKind::Parameter);
                }
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
        self.push_body_receiver(ty, Some(label), SuperOf::Class(class));
        pushed + 1
    }

    fn class(&mut self, node: SyntaxNode<'_>) {
        let pushed = match self.item_of(node) {
            Some(class) => self.push_class_receivers(class),
            None => 0,
        };
        self.push_scope_of(ScopeKind::ConstructorParameters);
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

    /// The parameters are all declared before any default value is
    /// inferred: a name in a default means a parameter — even one declared
    /// after it — before a member of the class.
    fn constructor_parameters(&mut self, node: SyntaxNode<'_>) {
        let parameters: Vec<SyntaxNode<'_>> = named_children(node)
            .filter(|child| child.kind() == "class_parameter")
            .collect();
        let mut types = Vec::with_capacity(parameters.len());
        for parameter in &parameters {
            let ty = self
                .type_child(*parameter)
                .map(|written| self.written_type(written))
                .unwrap_or(Ty::Unknown);
            if let Some(name) =
                named_children(*parameter).find(|child| child.kind() == "simple_identifier")
            {
                self.declare_node(name, ty.clone(), LocalKind::Parameter);
            }
            types.push(ty);
        }
        for (parameter, ty) in parameters.iter().zip(&types) {
            let default = named_children(*parameter)
                .skip_while(|child| !syntax::ast::kotlin::is_type(*child))
                .nth(1);
            if let Some(default) = default {
                self.expression(default, Some(ty));
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
        self.push_scope_of(ScopeKind::Function);
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
    /// A function's parameters, all declared before any default value is
    /// inferred (see [`Self::constructor_parameters`]).
    fn parameters(&mut self, node: SyntaxNode<'_>) {
        let mut vararg = false;
        let mut defaults: Vec<(SyntaxNode<'_>, Ty)> = Vec::new();
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
                _ => defaults.push((child, last_type.clone())),
            }
        }
        for (default, expected) in defaults {
            self.expression(default, Some(&expected));
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
        self.push_scope_of(ScopeKind::Function);
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
        // Smart casts in one member's initializer stay there.
        self.push_scope();
        let initialized = self.property_initializer(node, written.as_ref());
        self.pop_scope();
        let stored = written.clone().or(initialized.clone());
        let mut from_getter = None;
        for accessor in accessors(node) {
            match accessor.kind() {
                "getter" => from_getter = self.getter(accessor, stored.as_ref()),
                _ => self.setter(accessor, stored.as_ref()),
            }
        }
        if pushed_receiver {
            self.pop_receiver();
        }
        stored.or(from_getter).unwrap_or(Ty::Unknown)
    }

    /// A getter's body, with `field` in scope; an expression body's type
    /// (what an unwritten property type comes from).
    fn getter(&mut self, node: SyntaxNode<'_>, stored: Option<&Ty>) -> Option<Ty> {
        let body = named_children(node).find(|part| part.kind() == "function_body")?;
        self.push_scope_of(ScopeKind::Function);
        self.declare_backing_field(node, stored);
        let ty = self.function_body(body, stored);
        self.pop_scope();
        has_token(body, "=").then_some(ty)
    }

    /// `field` of the property whose accessor `accessor` is.
    fn declare_backing_field(&mut self, accessor: SyntaxNode<'_>, stored: Option<&Ty>) {
        let ty = stored.cloned().unwrap_or(Ty::Unknown);
        let at = TextRange::new(accessor.start_byte(), accessor.start_byte());
        self.declare(Name::new("field"), at, ty, LocalKind::BackingField);
    }

    fn setter(&mut self, node: SyntaxNode<'_>, stored: Option<&Ty>) {
        self.push_scope_of(ScopeKind::Function);
        self.declare_backing_field(node, stored);
        let property_type = stored.cloned();
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
            _ => {
                let ty = self.expression(node, None);
                self.narrow_after_contract(node);
                ty
            }
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

/// A script (`.kts`): statements at the top level, which a regular file
/// can't have.
fn is_script(parse: &Parse) -> bool {
    named_children(parse.root()).any(|child| {
        !matches!(
            child.kind(),
            "package_header"
                | "import_list"
                | "import_header"
                | "file_annotation"
                | "shebang_line"
                | "line_comment"
                | "multiline_comment"
                | "class_declaration"
                | "object_declaration"
                | "function_declaration"
                | "property_declaration"
                | "type_alias"
        )
    })
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

/// A property's accessors: the grammar puts a `get()`/`set()` on its own
/// line after the declaration, as its sibling; on the declaration's line,
/// as its child.
fn accessors(property: SyntaxNode<'_>) -> Vec<SyntaxNode<'_>> {
    let is_accessor = |node: &SyntaxNode<'_>| matches!(node.kind(), "getter" | "setter");
    let mut found: Vec<SyntaxNode<'_>> = named_children(property).filter(is_accessor).collect();
    let mut next = property.next_named_sibling();
    while let Some(sibling) = next.filter(is_accessor) {
        found.push(sibling);
        next = sibling.next_named_sibling();
    }
    found
}

/// What a function's result type comes from.
enum FunctionResult<'tree> {
    /// `: T`.
    Written(SyntaxNode<'tree>),
    /// `{ … }` without `: T`: `Unit`, whatever the body does.
    Block,
    /// `= expression`: inferred from the body.
    Expression,
    /// No body (abstract) and no type.
    Missing,
}

fn function_result(node: SyntaxNode<'_>) -> FunctionResult<'_> {
    if let Some(written) = FunctionDeclaration::new_unchecked(node).return_type() {
        return FunctionResult::Written(written);
    }
    match named_children(node).find(|child| child.kind() == "function_body") {
        Some(body) if has_token(body, "{") => FunctionResult::Block,
        Some(_) => FunctionResult::Expression,
        None => FunctionResult::Missing,
    }
}
