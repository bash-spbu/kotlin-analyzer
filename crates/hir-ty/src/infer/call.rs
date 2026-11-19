//! Calls: the candidates a callee name has, level by level; matching
//! arguments to parameters; choosing among overloads; inferring the
//! callee's type parameters; typing lambda arguments from the parameter
//! they're passed to.

use std::sync::Arc;

use hir_def::ItemLocation;
use hir_def::item_tree::{ItemKind, Modifiers, Signature};
use hir_def::resolver::{Resolution, resolve_type_path};
use intern::{Name, Path};
use syntax::{SyntaxNode, has_token, named_children};

use super::declarations::Inferrer;
use super::expressions::Typed;
use super::fit::Fit;
use super::{ArgumentMismatch, Certainty, LocalId, NameSite, Reference};
use crate::lower::{TypeScope, lower_type};
use crate::members::{Member, class_substitution};
use crate::signature::{Callable, CallableSignature, ParameterSignature};
use crate::ty::{FunctionType, Nullability, Substitution, Ty, TypeParameterId};

/// An argument of a call, typed when the call's resolution needs it.
pub(super) struct Argument<'tree> {
    name: Option<Name>,
    /// The `name` of `name = value`, as written.
    label: Option<SyntaxNode<'tree>>,
    value: ArgumentValue<'tree>,
    spread: bool,
    /// After the parentheses: always the last parameter.
    trailing: bool,
    /// The value as written, where a mismatch is shown.
    written: Option<SyntaxNode<'tree>>,
}

enum ArgumentValue<'tree> {
    Typed {
        ty: Ty,
        integer_literal: bool,
    },
    /// An expression not inferred yet: inferred with the parameter's type
    /// as expected once the callee is known (if it's known up front).
    Pending(SyntaxNode<'tree>),
    /// A lambda or anonymous function: inferred last, its parameters typed
    /// from the parameter it's passed to.
    Lambda(SyntaxNode<'tree>),
}

impl Argument<'_> {
    fn typed(ty: Ty) -> Argument<'static> {
        Argument {
            name: None,
            label: None,
            value: ArgumentValue::Typed {
                ty,
                integer_literal: false,
            },
            spread: false,
            trailing: false,
            written: None,
        }
    }
}

/// One overload a call may mean.
#[derive(Clone)]
struct Candidate {
    callable: Callable,
    signature: Arc<CallableSignature>,
    /// What the receiver binds: a member's class's type parameters.
    substitution: Substitution,
    /// The value an extension is called on (explicit or implicit).
    receiver: Option<Ty>,
    /// The value a member is called on.
    dispatch: Option<Ty>,
}

/// A candidate whose parameters the arguments can be matched to: for each
/// argument, the index of its parameter.
struct Shaped {
    candidate: Candidate,
    parameter_of: Vec<usize>,
}

/// Where a call's candidates come from, in the order Kotlin looks.
enum Level {
    /// On a nullable receiver called with a plain `.`: the extensions (of
    /// any level) declared on a nullable receiver. Kotlin calls one of
    /// those before a member, which would need the receiver non-null.
    NullableReceiverExtensions(Ty),
    /// Member functions of a receiver's type.
    Members(Ty),
    /// Constructors of the `inner` classes of a receiver's type
    /// (`outer.Inner()`).
    InnerConstructors(Ty),
    /// Extension functions on a receiver's type declared as members of an
    /// implicit receiver's class (`private fun Map<…>.toList()` in a
    /// class, used inside it).
    MemberExtensions(Ty),
    /// Extension functions on a receiver's type from one of the file's
    /// top-level levels.
    Extensions { receiver: Ty, level: usize },
    /// Constructors of a class.
    Constructors(ItemLocation),
    /// Top-level functions (no receiver) from one of the file's levels.
    TopLevelFunctions(usize),
    /// Java static methods and nested classes' constructors.
    Statics(ItemLocation),
    /// Functions and classes a qualified package name gives.
    Declarations(Vec<ItemLocation>),
}

/// The number of top-level levels `top_level_levels` gives.
const TOP_LEVEL_LEVELS: usize = 5;

impl<'db> Inferrer<'db> {
    /// A call expression.
    pub(super) fn call(&mut self, node: SyntaxNode<'_>, expected: Option<&Ty>) -> Ty {
        let mut children = named_children(node);
        let (Some(callee), Some(suffix)) = (children.next(), children.next()) else {
            return Ty::Unknown;
        };
        let callee = super::reassociation::chain_operand(callee).unwrap_or(callee);
        let (mut arguments, type_arguments) = self.arguments_of(suffix);
        match callee.kind() {
            "simple_identifier" => {
                self.simple_call(callee, &mut arguments, &type_arguments, expected)
            }
            "navigation_expression" => {
                self.qualified_call(callee, &mut arguments, &type_arguments, expected)
            }
            _ => {
                let callee_type = self.expression(callee, None);
                let function = match callee_type {
                    Ty::Function(function) => Some(*function),
                    _ => None,
                };
                self.invoke(function, &mut arguments)
            }
        }
    }

    /// `name(…)`: a local function value, or what the scope levels have.
    fn simple_call(
        &mut self,
        callee: SyntaxNode<'_>,
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
    ) -> Ty {
        if self.probed(callee) {
            self.capture(None);
        }
        let name = self.name(callee);
        if let Some(local) = self.lookup_local(name) {
            let local_type = self.local_type(local);
            let function = match &local_type {
                Ty::Function(function) => Some((**function).clone()),
                _ => None,
            };
            let result = self.invoke(function, arguments);
            self.record(callee, Reference::Local(local), result.clone());
            return result;
        }
        let levels = self.simple_levels(name, callee.start_byte());
        // A function and a property of function type may share the name
        // (`fun notFound()` next to `val notFound: (InputStream) -> R`):
        // the property is called when no function takes the arguments.
        if let Some((location, function)) = self.function_property_in_scope(name)
            && function.parameters.len() == arguments.len()
            && !self.any_shape_fits(&levels, name, arguments)
        {
            let result = self.invoke(Some(function), arguments);
            self.record(callee, Reference::Declaration(location), result.clone());
            return result;
        }
        if let Some(result) = self.resolve(
            Some(callee),
            name,
            NameSite::Scope,
            levels,
            arguments,
            type_arguments,
            expected,
        ) {
            return result;
        }
        // A property of function type, called: `handler(event)`.
        if let Typed::Value(Ty::Function(function)) = self.identifier(callee) {
            return self.invoke(Some(*function), arguments);
        }
        self.type_remaining(arguments);
        Ty::Unknown
    }

    /// The innermost implicit receiver's property `name`, when its type is
    /// a function type.
    fn function_property_in_scope(&mut self, name: Name) -> Option<(ItemLocation, FunctionType)> {
        let receivers: Vec<Ty> = (0..self.receivers.len())
            .rev()
            .map(|index| self.receiver_type(index))
            .collect();
        for receiver in receivers {
            if let Some((location, ty)) = self.property_of(&receiver, name) {
                return match ty {
                    Ty::Function(function) => Some((location, *function)),
                    _ => None,
                };
            }
        }
        None
    }

    /// Whether any function `levels` give for `name` takes the arguments'
    /// shape (count, names).
    fn any_shape_fits(&mut self, levels: &[Level], name: Name, arguments: &[Argument<'_>]) -> bool {
        levels.iter().any(|level| {
            self.level_candidates(level, name)
                .iter()
                .any(|candidate| match_arguments(&candidate.signature, arguments).is_some())
        })
    }

    /// `receiver.name(…)` and `receiver?.name(…)`.
    fn qualified_call(
        &mut self,
        callee: SyntaxNode<'_>,
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
    ) -> Ty {
        let mut parts = named_children(callee);
        let (Some(receiver_node), Some(suffix)) = (parts.next(), parts.next()) else {
            self.type_remaining(arguments);
            return Ty::Unknown;
        };
        let name_node = named_children(suffix).find(|child| child.kind() == "simple_identifier");
        let member = name_node.map(|name| self.name(name));
        let receiver = self.member_receiver(receiver_node, member);
        let safe = has_token(suffix, "?.");
        // `x?.let { … x … }`: past the receiver, the call runs only when
        // `x` isn't null.
        // `x?.f(…)`: two ways on — `x` was null, or the call ran with it not.
        let safely_called = safe.then(|| self.not_null_chain(receiver_node));
        let null_way = safely_called.as_ref().map(|_| self.flow.clone());
        if let Some(not_null) = &safely_called {
            self.narrow(not_null);
        }
        let ty = self.qualified_call_on(
            receiver_node,
            suffix,
            receiver,
            name_node,
            arguments,
            type_arguments,
            expected,
        );
        if let Some(null_way) = null_way {
            let call_way = std::mem::take(&mut self.flow);
            self.flow = self.merge(vec![null_way, call_way]);
        }
        ty
    }

    /// `receiver.name(arguments)` once `receiver` is inferred.
    #[allow(clippy::too_many_arguments)]
    fn qualified_call_on(
        &mut self,
        receiver_node: SyntaxNode<'_>,
        suffix: SyntaxNode<'_>,
        receiver: Typed,
        name_node: Option<SyntaxNode<'_>>,
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
    ) -> Ty {
        let safe = has_token(suffix, "?.");
        self.check_safe_call(receiver_node, &receiver, suffix);
        let Some(name_node) = name_node else {
            self.type_remaining(arguments);
            return Ty::Unknown;
        };
        if self.probed(name_node) {
            self.capture(Some(&receiver));
        }
        let name = self.name(name_node);
        // `block.invoke(…)` / `block?.invoke(…)` calls the function value.
        if let Typed::Value(Ty::Function(function)) = &receiver
            && name.as_str() == "invoke"
        {
            let result = self.invoke(Some((**function).clone()), arguments);
            return if safe { result.nullable() } else { result };
        }
        let levels = match &receiver {
            Typed::Value(Ty::Unknown) => {
                self.type_remaining(arguments);
                return Ty::Unknown;
            }
            Typed::Value(receiver_type) => {
                let mut levels = Vec::new();
                if !safe && receiver_type.nullability() == Nullability::Nullable {
                    levels.push(Level::NullableReceiverExtensions(receiver_type.clone()));
                }
                // `x?.f()` calls `f` on `x` when it isn't null.
                let called_on = if safe {
                    receiver_type.clone().without_question_mark()
                } else {
                    receiver_type.clone()
                };
                levels.extend(self.receiver_levels(&called_on));
                levels
            }
            Typed::Class(class) => self.static_levels(*class),
            Typed::Package(package) => {
                let found = hir_def::def_map::visible_top_level(self.db, self.file, package, name);
                vec![Level::Declarations(found)]
            }
        };
        // A member property of function type is called before an extension
        // function of the name (`builder.test(scope, client)` for `var
        // test: suspend Scope.(Client) -> Unit`, its receiver the first
        // argument) — when no function takes the arguments.
        let invocable_property = match &receiver {
            Typed::Value(receiver_type) => match self.property_of(receiver_type, name) {
                Some((location, Ty::Function(function))) => Some((location, function)),
                _ => None,
            },
            _ => None,
        };
        if let Some((location, function)) = &invocable_property
            && function.parameters.len() + usize::from(function.receiver.is_some())
                == arguments.len()
            && !self.any_shape_fits(&levels, name, arguments)
        {
            let (location, function) = (*location, function.clone());
            let result = self.invoke(Some(*function), arguments);
            self.record(name_node, Reference::Declaration(location), result.clone());
            return if safe { result.nullable() } else { result };
        }
        // A function of the name was taken over a property of function type
        // by the arguments' shape; Kotlin also weighs their types and may
        // invoke the property (`config.transform(node)` for `var transform:
        // (Node) -> String` beside `fun transform(block)`).
        let property_may_be_invoked = invocable_property.is_some();
        let site = match &receiver {
            Typed::Value(receiver_type) => NameSite::Member(receiver_type.clone()),
            Typed::Class(_) => NameSite::Static,
            Typed::Package(_) => NameSite::Package,
        };
        let result = self.resolve(
            Some(name_node),
            name,
            site,
            levels,
            arguments,
            type_arguments,
            expected,
        );
        if property_may_be_invoked {
            self.mark_call_certainty(Some(name_node), false);
        }
        if !safe
            && let Typed::Value(receiver_type) = &receiver
            && let Some(resolved) = self.recorded_at(name_node)
            && let Reference::Declaration(location) = resolved.reference
        {
            self.check_receiver_not_null(receiver_node, receiver_type, name_node, location);
        }
        let result = match result {
            Some(result) => result,
            None => {
                let property = match &receiver {
                    Typed::Value(receiver_type) => self.property_of(receiver_type, name),
                    _ => None,
                };
                match property {
                    Some((location, Ty::Function(function))) => {
                        let result = self.invoke(Some(*function), arguments);
                        self.record(name_node, Reference::Declaration(location), result.clone());
                        result
                    }
                    _ if let Some((local, function)) = self.local_extension(&receiver, name) => {
                        let result = self.invoke(Some(function), arguments);
                        self.record(name_node, Reference::Local(local), result.clone());
                        result
                    }
                    _ if let Some((location, function)) =
                        self.receiver_property_extension(&receiver, name) =>
                    {
                        let result = self.invoke(Some(function), arguments);
                        self.record(name_node, Reference::Declaration(location), result.clone());
                        result
                    }
                    _ => {
                        self.type_remaining(arguments);
                        let site = match &receiver {
                            Typed::Value(Ty::Function(_)) => None,
                            Typed::Value(receiver_type) => {
                                Some(NameSite::Member(receiver_type.clone()))
                            }
                            Typed::Class(_) => Some(NameSite::Static),
                            Typed::Package(_) => Some(NameSite::Package),
                        };
                        if let Some(site) = site {
                            self.record_unresolved(name_node, site);
                        }
                        Ty::Unknown
                    }
                }
            }
        };
        if safe { result.nullable() } else { result }
    }

    /// `left name right`.
    pub(super) fn infix_call(
        &mut self,
        receiver: Ty,
        function: SyntaxNode<'_>,
        right: SyntaxNode<'_>,
    ) -> Ty {
        let name = self.name(function);
        let mut arguments = vec![Argument {
            name: None,
            label: None,
            value: ArgumentValue::Pending(right),
            spread: false,
            trailing: false,
            written: Some(right),
        }];
        if receiver.is_unknown() {
            self.type_remaining(&mut arguments);
            return Ty::Unknown;
        }
        let levels = self.receiver_levels(&receiver);
        let site = NameSite::Member(receiver.clone());
        match self.resolve(
            Some(function),
            name,
            site,
            levels,
            &mut arguments,
            &[],
            None,
        ) {
            Some(result) => result,
            None => {
                self.type_remaining(&mut arguments);
                self.record_unresolved(function, NameSite::Member(receiver));
                Ty::Unknown
            }
        }
    }

    /// The result of the operator function `name` of a value of type
    /// `receiver` given arguments of `argument_types` (`plus`, `get`,
    /// `iterator`, `componentN`, `getValue`), if one fits.
    pub(super) fn operator_result(
        &mut self,
        receiver: &Ty,
        name: &str,
        argument_types: Vec<Ty>,
    ) -> Option<Ty> {
        if receiver.is_unknown() {
            return None;
        }
        let name = Name::new(name);
        let mut arguments: Vec<Argument<'_>> =
            argument_types.into_iter().map(Argument::typed).collect();
        let levels = self.receiver_levels(receiver);
        let site = NameSite::Member(receiver.clone());
        let result = self.resolve(None, name, site, levels, &mut arguments, &[], None);
        result.or_else(|| self.data_class_component(receiver, name))
    }

    /// `componentN()` of a workspace data class: its N-th constructor
    /// property.
    fn data_class_component(&self, receiver: &Ty, name: Name) -> Option<Ty> {
        let index: usize = name.as_str().strip_prefix("component")?.parse().ok()?;
        let class_type = receiver.class()?;
        let tree = hir_def::item_tree(self.db, class_type.class.container);
        let class = tree.item(class_type.class.item);
        if !class.modifiers.contains(Modifiers::DATA) {
            return None;
        }
        let Signature::Class {
            primary_constructor: Some(parameters),
            ..
        } = &class.signature
        else {
            return None;
        };
        let parameter = parameters.get(index.checked_sub(1)?)?;
        let scope = TypeScope::of_declaration(self.db, self.file, class_type.class);
        let ty = lower_type(self.db, scope, parameter.type_ref.as_ref()?);
        Some(ty.substitute(&class_substitution(class_type)))
    }

    // --- candidates -------------------------------------------------------

    /// The levels of an unqualified call: each implicit receiver's members,
    /// member extensions another implicit receiver's class declares for it
    /// (`with(template) { apply() }` for `fun Outer.apply()` in
    /// `Template`), and extensions, innermost first; constructors of the
    /// class the name means; top-level functions.
    fn simple_levels(&mut self, name: Name, offset: usize) -> Vec<Level> {
        let mut levels = Vec::new();
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receiver_type(index);
            levels.push(Level::Members(receiver.clone()));
            levels.push(Level::MemberExtensions(receiver.clone()));
            for level in 0..TOP_LEVEL_LEVELS {
                levels.push(Level::Extensions {
                    receiver: receiver.clone(),
                    level,
                });
            }
        }
        match resolve_type_path(self.db, self.file, &Path::new([name]), offset) {
            Resolution::Declaration(class) => levels.push(Level::Constructors(class)),
            Resolution::Ambiguous(classes) => {
                for class in classes {
                    levels.push(Level::Constructors(class));
                }
            }
            _ => {}
        }
        for level in 0..TOP_LEVEL_LEVELS {
            levels.push(Level::TopLevelFunctions(level));
        }
        levels
    }

    fn receiver_levels(&self, receiver: &Ty) -> Vec<Level> {
        let mut levels = vec![
            Level::Members(receiver.clone()),
            Level::InnerConstructors(receiver.clone()),
            Level::MemberExtensions(receiver.clone()),
        ];
        for level in 0..TOP_LEVEL_LEVELS {
            levels.push(Level::Extensions {
                receiver: receiver.clone(),
                level,
            });
        }
        levels
    }

    /// A local extension function `fun T.name(…)` declared in the body,
    /// called on a value: its type, the receiver taken.
    /// An implicit receiver's property of extension function type the
    /// explicit receiver can be called with: `origin.content()` for
    /// `val content: HttpResponse.() -> ByteReadChannel` of the class.
    fn receiver_property_extension(
        &mut self,
        receiver: &Typed,
        name: Name,
    ) -> Option<(ItemLocation, FunctionType)> {
        let Typed::Value(receiver_type) = receiver else {
            return None;
        };
        let (location, function) = self.function_property_in_scope(name)?;
        let declared_receiver = function.receiver.as_ref()?;
        let receiver_fits = self.types.fits(receiver_type, declared_receiver) != Fit::No;
        receiver_fits.then_some((location, function))
    }

    fn local_extension(&self, receiver: &Typed, name: Name) -> Option<(LocalId, FunctionType)> {
        let Typed::Value(receiver_type) = receiver else {
            return None;
        };
        let local = self.lookup_local(name)?;
        let Ty::Function(function) = self.local_type(local) else {
            return None;
        };
        let declared_receiver = function.receiver.as_ref()?;
        let receiver_fits = self.types.fits(receiver_type, declared_receiver) != Fit::No;
        receiver_fits.then_some((local, *function))
    }

    /// `Class.name(…)`: statics and nested classes, then the companion's
    /// (or the object's own) members and extensions.
    fn static_levels(&self, class: ItemLocation) -> Vec<Level> {
        let mut levels = vec![Level::Statics(class)];
        if let Some(instance) = self.types.object_instance(class) {
            levels.extend(self.receiver_levels(&Ty::Class(instance)));
        }
        levels
    }

    fn level_candidates(&mut self, level: &Level, name: Name) -> Vec<Candidate> {
        match level {
            Level::Members(receiver) => {
                let members = self.types.members_named(receiver, name);
                let mut candidates: Vec<Candidate> = members
                    .into_iter()
                    .filter(|member| self.item_kind(member.location) == ItemKind::Function)
                    .filter_map(|member| self.member_candidate(member, receiver))
                    .collect();
                if candidates.is_empty()
                    && let Some(class_type) = receiver.class()
                {
                    // `copy` of a data class.
                    let substitution = crate::members::class_substitution(class_type);
                    for callable in self.types.generated_functions(class_type.class, name) {
                        if let Some(signature) = self.types.signature(callable) {
                            candidates.push(Candidate {
                                callable,
                                signature,
                                substitution: substitution.clone(),
                                receiver: None,
                                dispatch: None,
                            });
                        }
                    }
                }
                candidates
            }
            Level::MemberExtensions(receiver) => {
                let mut candidates = Vec::new();
                for index in (0..self.receivers.len()).rev() {
                    let dispatch = self.receiver_type(index);
                    for member in self.types.members_named(&dispatch, name) {
                        if self.item_kind(member.location) != ItemKind::Function {
                            continue;
                        }
                        let Some(signature) =
                            self.types.signature(Callable::Function(member.location))
                        else {
                            continue;
                        };
                        let Some(declared) = &signature.receiver else {
                            continue;
                        };
                        let declared = declared.substitute(&member.substitution);
                        if self.types.fits(&receiver.clone().non_null(), &declared) == Fit::No {
                            continue;
                        }
                        candidates.push(Candidate {
                            callable: Callable::Function(member.location),
                            signature: signature.clone(),
                            substitution: member.substitution,
                            receiver: Some(receiver.clone()),
                            dispatch: None,
                        });
                    }
                }
                candidates
            }
            Level::Extensions { receiver, level } => {
                let levels = self.top_level(name);
                let Some(declarations) = levels.get(*level) else {
                    return Vec::new();
                };
                declarations
                    .iter()
                    .filter_map(|location| self.extension_candidate(*location, receiver))
                    .collect()
            }
            Level::NullableReceiverExtensions(receiver) => {
                let mut found =
                    self.level_candidates(&Level::MemberExtensions(receiver.clone()), name);
                for level in 0..TOP_LEVEL_LEVELS {
                    let extensions = Level::Extensions {
                        receiver: receiver.clone(),
                        level,
                    };
                    found.extend(self.level_candidates(&extensions, name));
                }
                found.retain(|candidate| {
                    candidate
                        .signature
                        .receiver
                        .as_ref()
                        .is_some_and(|declared| declared.nullability() == Nullability::Nullable)
                });
                found
            }
            Level::InnerConstructors(receiver) => self
                .types
                .inner_classes_named(receiver, name)
                .into_iter()
                .flat_map(|class| self.constructor_candidates(class))
                .collect(),
            Level::Constructors(class) => {
                let mut candidates = self.constructor_candidates(*class);
                if let Some(sam_constructor) = self.types.sam_constructor(*class)
                    && let Some(signature) = self.types.signature(sam_constructor)
                {
                    candidates.push(Candidate {
                        callable: sam_constructor,
                        signature,
                        substitution: Substitution::default(),
                        receiver: None,
                        dispatch: None,
                    });
                }
                candidates.extend(self.invoke_candidates(*class));
                candidates
            }
            Level::TopLevelFunctions(level) => {
                let levels = self.top_level(name);
                let Some(declarations) = levels.get(*level) else {
                    return Vec::new();
                };
                let declarations = declarations.clone();
                self.plain_candidates(&declarations)
            }
            Level::Statics(class) => {
                let statics = self.types.static_members_named(*class, name);
                let mut candidates = self.plain_candidates(&statics);
                // `valueOf` and `values` of an enum.
                for callable in self.types.generated_functions(*class, name) {
                    if let Some(signature) = self.types.signature(callable) {
                        candidates.push(Candidate {
                            callable,
                            signature,
                            substitution: Substitution::default(),
                            receiver: None,
                            dispatch: None,
                        });
                    }
                }
                candidates
            }
            Level::Declarations(declarations) => {
                let declarations = declarations.clone();
                self.plain_candidates(&declarations)
            }
        }
    }

    /// Functions without a receiver, and constructors of the classes among
    /// `declarations`.
    fn plain_candidates(&mut self, declarations: &[ItemLocation]) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        for location in declarations {
            match self.item_kind(*location) {
                ItemKind::Function => {
                    let Some(signature) = self.types.signature(Callable::Function(*location))
                    else {
                        continue;
                    };
                    if signature.receiver.is_some() {
                        continue;
                    }
                    candidates.push(Candidate {
                        callable: Callable::Function(*location),
                        signature,
                        substitution: Substitution::default(),
                        receiver: None,
                        dispatch: None,
                    });
                }
                kind if kind.is_classifier() => {
                    candidates.extend(self.constructor_candidates(*location))
                }
                _ => {}
            }
        }
        candidates
    }

    fn member_candidate(&self, member: Member, receiver: &Ty) -> Option<Candidate> {
        let mut signature = self.types.signature(Callable::Function(member.location))?;
        if signature.receiver.is_some() {
            // A member extension needs both receivers; not modelled.
            return None;
        }
        // An override takes its default values from what it overrides.
        let inherited = self.types.inherited_defaults(receiver, member.location);
        if inherited.iter().any(|has_default| *has_default) {
            let mut with_defaults = (*signature).clone();
            for (parameter, inherited) in with_defaults.parameters.iter_mut().zip(inherited) {
                parameter.has_default |= inherited;
            }
            signature = Arc::new(with_defaults);
        }
        Some(Candidate {
            callable: Callable::Function(member.location),
            signature,
            substitution: member.substitution,
            receiver: None,
            dispatch: Some(receiver.clone()),
        })
    }

    fn extension_candidate(&self, location: ItemLocation, receiver: &Ty) -> Option<Candidate> {
        if self.item_kind(location) != ItemKind::Function {
            return None;
        }
        let signature = self.types.signature(Callable::Function(location))?;
        let declared = signature.receiver.as_ref()?;
        // An extension on a type parameter (`T.let`, `A.to`) takes the
        // receiver as it is: `T` is `X?` for an `X?`.
        let declared_takes_nullable = declared.nullability() == crate::Nullability::Nullable
            || matches!(declared, Ty::TypeParameter { .. });
        let value = if declared_takes_nullable {
            receiver.clone()
        } else {
            receiver.clone().without_question_mark()
        };
        if self.types.fits(&value, declared) == Fit::No {
            return None;
        }
        Some(Candidate {
            callable: Callable::Function(location),
            signature,
            substitution: Substitution::default(),
            receiver: Some(value),
            dispatch: None,
        })
    }

    /// `Name(…)` of an object, or of a class with a companion, that has an
    /// `operator fun invoke` — a member, or an extension on it in scope
    /// (`operator fun SavedStateHandle.Companion.invoke(route)`): calls that.
    fn invoke_candidates(&mut self, class: ItemLocation) -> Vec<Candidate> {
        let Some(instance) = self.types.object_instance(class) else {
            return Vec::new();
        };
        let instance = Ty::Class(instance);
        let invoke = Name::new("invoke");
        let mut candidates: Vec<Candidate> = self
            .types
            .members_named(&instance, invoke)
            .into_iter()
            .filter(|member| self.item_kind(member.location) == ItemKind::Function)
            .filter_map(|member| self.member_candidate(member, &instance))
            .collect();
        for level in 0..TOP_LEVEL_LEVELS {
            let extensions = Level::Extensions {
                receiver: instance.clone(),
                level,
            };
            candidates.extend(self.level_candidates(&extensions, invoke));
        }
        candidates
    }

    /// The constructors of `class` (a type alias's class's, through the
    /// alias).
    fn constructor_candidates(&self, class: ItemLocation) -> Vec<Candidate> {
        let class = match self.item_kind(class) {
            ItemKind::TypeAlias => {
                let scope = TypeScope::of_declaration(self.db, self.file, class);
                let tree = hir_def::item_tree(self.db, class.container);
                let Signature::TypeAlias { target } = &tree.item(class.item).signature else {
                    return Vec::new();
                };
                match lower_type(self.db, scope, target) {
                    Ty::Class(target) => target.class,
                    _ => return Vec::new(),
                }
            }
            _ => class,
        };
        self.types
            .constructors(class)
            .into_iter()
            .filter_map(|callable| {
                Some(Candidate {
                    callable,
                    signature: self.types.signature(callable)?,
                    substitution: Substitution::default(),
                    receiver: None,
                    dispatch: None,
                })
            })
            .collect()
    }

    // --- resolution -------------------------------------------------------

    /// Resolves a call among the candidates `levels` give: the first level
    /// with a candidate that fits wins. Records what `name_node` means and
    /// returns the call's type; `None` when nothing has the name (the
    /// arguments are then left untyped).
    #[allow(clippy::too_many_arguments)]
    fn resolve(
        &mut self,
        name_node: Option<SyntaxNode<'_>>,
        name: Name,
        site: NameSite,
        levels: Vec<Level>,
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
    ) -> Option<Ty> {
        let mut seen: Vec<ItemLocation> = Vec::new();
        let mut arguments_typed = false;
        // The one candidate of a level its arguments' shape fitted, when
        // their types didn't.
        let mut rejected: Option<(Candidate, Vec<usize>)> = None;
        for level in &levels {
            let candidates = self.level_candidates(level, name);
            for candidate in &candidates {
                let location = candidate.callable.location();
                if !seen.contains(&location) {
                    seen.push(location);
                }
            }
            let shaped: Vec<Shaped> = candidates
                .into_iter()
                .filter(|candidate| takes_type_arguments(candidate, type_arguments))
                .filter_map(|candidate| {
                    let parameter_of = match_arguments(&candidate.signature, arguments)?;
                    Some(Shaped {
                        candidate,
                        parameter_of,
                    })
                })
                .collect();
            if shaped.is_empty() {
                continue;
            }
            if !arguments_typed && shaped.len() == 1 {
                let only = shaped.into_iter().next().expect("one candidate");
                let completion =
                    self.complete(&only, arguments, type_arguments, expected, Some(name));
                arguments_typed = true;
                if let Some(result) = completion {
                    self.mark_call_certainty(name_node, true);
                    self.record_callee(
                        name_node,
                        Reference::Declaration(only.candidate.callable.location()),
                        &result,
                    );
                    self.record_labels(&only, arguments);
                    return Some(result);
                }
                rejected = Some((only.candidate, only.parameter_of));
                continue;
            }
            if !arguments_typed {
                self.type_pending(arguments);
                arguments_typed = true;
            }
            let applicable: Vec<Shaped> = shaped
                .into_iter()
                .filter(|shaped| self.arguments_fit(shaped, arguments) != Fit::No)
                .collect();
            if applicable.is_empty() {
                continue;
            }
            let several_applicable = applicable.len() > 1;
            let best = self.most_specific(applicable, arguments);
            if let [only] = &best[..] {
                // Chosen among several by specificity, which isn't
                // modelled as fully as the compiler's.
                self.mark_call_certainty(name_node, !several_applicable);
                let result = self
                    .complete(only, arguments, type_arguments, expected, Some(name))
                    .unwrap_or(Ty::Unknown);
                self.record_labels(only, arguments);
                self.record_callee(
                    name_node,
                    Reference::Declaration(only.candidate.callable.location()),
                    &result,
                );
                return Some(result);
            }
            let result = self.ambiguous(&best, arguments, type_arguments, expected, name);
            let locations: Box<[ItemLocation]> = best
                .iter()
                .map(|shaped| shaped.candidate.callable.location())
                .collect();
            self.record_callee(name_node, Reference::Candidates(locations), &result);
            return Some(result);
        }
        if seen.is_empty() {
            return None;
        }
        let no_shape_fits = !arguments_typed;
        self.type_remaining(arguments);
        let only_candidate = seen.len() == 1;
        if let (true, Some((candidate, parameter_of))) = (only_candidate, &rejected)
            && self.lookup_certainty(name, &site) == Certainty::Certain
        {
            self.check_arguments(candidate, parameter_of, arguments);
        }
        if let Some(name_node) = name_node
            && no_shape_fits
            && self.lookup_certainty(name, &site) == Certainty::Certain
        {
            self.record_argument_mismatch(ArgumentMismatch {
                range: syntax::TextRange::of(name_node),
                candidates: seen.clone().into_boxed_slice(),
            });
        }
        self.record_callee(
            name_node,
            Reference::Candidates(seen.into_boxed_slice()),
            &Ty::Unknown,
        );
        Some(Ty::Unknown)
    }

    /// Remembers a call whose resolution may not be the compiler's: its
    /// value isn't judged against what it's passed to.
    fn mark_call_certainty(&mut self, name_node: Option<SyntaxNode<'_>>, certain: bool) {
        let Some(name_node) = name_node else {
            return;
        };
        let start = name_node.start_byte() as u32;
        if certain {
            self.uncertain_calls.remove(&start);
        } else {
            self.uncertain_calls.insert(start);
        }
    }

    /// The arguments of the one function a call can mean whose classes
    /// can't be their parameters' — parameters whose types don't depend on
    /// the call (no type parameter of the function's or unbound of its
    /// class's), not `vararg`.
    fn check_arguments(
        &mut self,
        candidate: &Candidate,
        parameter_of: &[usize],
        arguments: &[Argument<'_>],
    ) {
        // A multiplatform `expect` (or `actual`) declaration may have
        // counterparts and extensions this resolution doesn't weigh.
        if self.is_multiplatform(candidate.callable.location()) {
            return;
        }
        for (argument, parameter_index) in arguments.iter().zip(parameter_of) {
            let parameter = &candidate.signature.parameters[*parameter_index];
            let (ArgumentValue::Typed { ty, .. }, Some(written)) =
                (&argument.value, argument.written)
            else {
                continue;
            };
            let declared = parameter.ty.substitute(&candidate.substitution);
            let independent = !declared.has_type_parameter();
            if parameter.vararg || argument.spread || !independent {
                continue;
            }
            self.check_value(super::MismatchSite::Argument, written, ty, &declared);
        }
    }

    /// Whether `location` or a class around it is `expect` or `actual`.
    fn is_multiplatform(&self, location: ItemLocation) -> bool {
        let tree = hir_def::item_tree(self.db, location.container);
        let mut current = Some(location.item);
        while let Some(item) = current {
            let modifiers = tree.item(item).modifiers;
            let marked = modifiers.contains(hir_def::item_tree::Modifiers::EXPECT)
                || modifiers.contains(hir_def::item_tree::Modifiers::ACTUAL);
            if marked {
                return true;
            }
            current = tree.item(item).parent;
        }
        false
    }

    fn record_callee(
        &mut self,
        name_node: Option<SyntaxNode<'_>>,
        reference: Reference,
        result: &Ty,
    ) {
        if let Some(name_node) = name_node {
            self.record(name_node, reference, result.clone());
        }
    }

    /// Infers the call of `shaped`: binds its type parameters from the
    /// explicit type arguments, the receiver, the arguments (inferred here
    /// with their parameters' types expected) and the expected type; then
    /// infers the lambdas. `None` when an argument doesn't fit — the
    /// lambdas are then left for the next candidate.
    fn complete(
        &mut self,
        shaped: &Shaped,
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
        label: Option<Name>,
    ) -> Option<Ty> {
        let candidate = &shaped.candidate;
        let parameters = &candidate.signature.type_parameters;
        let mut substitution = candidate.substitution.clone();
        for (parameter, argument) in parameters.iter().zip(type_arguments) {
            substitution.insert(*parameter, argument.clone());
        }
        if let (Some(declared), Some(actual)) = (&candidate.signature.receiver, &candidate.receiver)
        {
            let declared = declared.substitute(&substitution);
            self.types
                .unify(&declared, actual, parameters, &mut substitution);
        }
        for (argument, parameter_index) in arguments.iter_mut().zip(&shaped.parameter_of) {
            let parameter = &candidate.signature.parameters[*parameter_index];
            if let ArgumentValue::Pending(node) = argument.value {
                let expected_argument = if argument.spread {
                    None
                } else {
                    Some(erase(&parameter.ty.substitute(&substitution), parameters))
                };
                let ty = self.expression(node, expected_argument.as_ref());
                argument.value = ArgumentValue::Typed {
                    integer_literal: is_integer_literal(node),
                    ty,
                };
            }
            if let ArgumentValue::Typed { ty, .. } = &argument.value
                && !argument.spread
            {
                let declared = parameter.ty.substitute(&substitution);
                self.types
                    .unify(&declared, ty, parameters, &mut substitution);
            }
        }
        if self.arguments_fit_with(shaped, arguments, &substitution) == Fit::No {
            return None;
        }
        if let Some(expected) = expected {
            let declared = candidate.signature.result.substitute(&substitution);
            self.types
                .unify(&declared, expected, parameters, &mut substitution);
        }
        for (argument, parameter_index) in arguments.iter_mut().zip(&shaped.parameter_of) {
            let ArgumentValue::Lambda(node) = argument.value else {
                continue;
            };
            let parameter = &candidate.signature.parameters[*parameter_index];
            let declared = parameter.ty.substitute(&substitution);
            let expected_lambda = erase(&declared, parameters);
            let ty = self.function_value(node, Some(&expected_lambda), label);
            // Against the declared `() -> T`, not `() -> X` with `T` already
            // bound: a lambda giving `X?` makes `T` nullable.
            self.types
                .unify(&parameter.ty, &ty, parameters, &mut substitution);
            argument.value = ArgumentValue::Typed {
                ty,
                integer_literal: false,
            };
        }
        // `<R, T : R> Result<T>.getOrElse(…): R`: `R` is at least `T` too,
        // which binding it from the arguments doesn't see — unknown.
        for parameter in parameters {
            let bounds_another = parameters.iter().any(|other| {
                other != parameter
                    && self
                        .types
                        .type_parameter_bounds(*other)
                        .iter()
                        .any(|bound| bound.mentions(*parameter))
            });
            if bounds_another {
                substitution.forget(*parameter);
            }
        }
        let result = erase(
            &candidate.signature.result.substitute(&substitution),
            parameters,
        );
        if !result.is_unknown() {
            return Some(result);
        }
        Some(
            self.approximated_result(candidate, &substitution)
                .unwrap_or(result),
        )
    }

    /// A member's result that is a type parameter a star in the type it's
    /// called on binds (`isEqualTo(…)`'s `SELF` on AssertJ's
    /// `AbstractStringAssert<?>`), approximated as the compiler captures
    /// the star when the receiver's class's parameter is that class's own
    /// type (`SELF : AbstractStringAssert<SELF>`): the type it's called on.
    fn approximated_result(
        &self,
        candidate: &Candidate,
        substitution: &Substitution,
    ) -> Option<Ty> {
        let Ty::TypeParameter {
            parameter,
            nullability,
        } = &candidate.signature.result
        else {
            return None;
        };
        let bound_to_star = substitution.get(*parameter).is_some_and(Ty::is_unknown);
        let Some(Ty::Class(receiver)) = candidate.dispatch.as_ref().filter(|_| bound_to_star)
        else {
            return None;
        };
        // Which of the receiver's class's own parameters the result's is.
        let own = self.types.self_type(receiver.class);
        let as_declaring = self.types.as_supertype(&Ty::Class(own), parameter.owner)?;
        let Ty::TypeParameter {
            parameter: receiver_parameter,
            ..
        } = as_declaring.arguments.get(parameter.index as usize)?
        else {
            return None;
        };
        let starred = receiver
            .arguments
            .get(receiver_parameter.index as usize)
            .is_some_and(Ty::is_unknown);
        if receiver_parameter.owner != receiver.class || !starred {
            return None;
        }
        let bound = self
            .types
            .type_parameter_bounds(*receiver_parameter)
            .into_iter()
            .next()?;
        // Only a class's own type: a star and a type that didn't resolve
        // are both unknown here, and another bound (`Any`) would say less
        // than nothing.
        bound
            .mentions(*receiver_parameter)
            .then(|| Ty::Class(receiver.clone()).with_nullability(*nullability))
    }

    /// What each `name =` of the chosen call names: a constructor's
    /// property (`val` parameter) when there is one, else the callee.
    fn record_labels(&mut self, shaped: &Shaped, arguments: &[Argument<'_>]) {
        for (argument, index) in arguments.iter().zip(&shaped.parameter_of) {
            let Some(label) = argument.label else {
                continue;
            };
            let parameter = &shaped.candidate.signature.parameters[*index];
            let class = match shaped.candidate.callable {
                Callable::Constructor { class, .. } | Callable::Generated { class, .. } => {
                    Some(class)
                }
                Callable::Function(_) => None,
            };
            let property = class.and_then(|class| {
                let tree = hir_def::item_tree(self.db, class.container);
                tree.named(Some(class.item), parameter.name)
                    .iter()
                    .find(|child| matches!(tree.item(**child).kind, ItemKind::Property { .. }))
                    .map(|child| ItemLocation {
                        container: class.container,
                        item: *child,
                    })
            });
            let target = property.unwrap_or(shaped.candidate.callable.location());
            let ty = parameter.ty.substitute(&shaped.candidate.substitution);
            self.record(label, Reference::Declaration(target), ty);
        }
    }

    /// Several overloads fit equally: the lambdas are inferred against the
    /// first; the call's type is theirs if they all agree.
    fn ambiguous(
        &mut self,
        best: &[Shaped],
        arguments: &mut [Argument<'_>],
        type_arguments: &[Ty],
        expected: Option<&Ty>,
        name: Name,
    ) -> Ty {
        let results: Vec<Ty> = best
            .iter()
            .map(|shaped| {
                erase(
                    &shaped
                        .candidate
                        .signature
                        .result
                        .substitute(&shaped.candidate.substitution),
                    &shaped.candidate.signature.type_parameters,
                )
            })
            .collect();
        // Erasing type parameters loses `T` vs `T?`: compare that apart.
        let nullabilities: Vec<Nullability> = best
            .iter()
            .map(|shaped| shaped.candidate.signature.result.nullability())
            .collect();
        let first = self
            .complete(&best[0], arguments, type_arguments, expected, Some(name))
            .unwrap_or(Ty::Unknown);
        self.type_remaining(arguments);
        let results_agree = results.iter().all(|result| *result == results[0]);
        let nullabilities_agree = nullabilities
            .iter()
            .all(|nullability| *nullability == nullabilities[0]);
        if results_agree && nullabilities_agree {
            first
        } else {
            Ty::Unknown
        }
    }

    /// Of the candidates that fit, those no other is more specific than;
    /// then a non-`vararg` one over a `vararg` one.
    fn most_specific(&self, applicable: Vec<Shaped>, arguments: &[Argument<'_>]) -> Vec<Shaped> {
        // An extension's receiver counts as its first parameter:
        // `List<T>.first()` is more specific than `Iterable<T>.first()`.
        let parameter_types: Vec<Vec<Option<Ty>>> =
            applicable
                .iter()
                .map(|shaped| {
                    let receiver = shaped.candidate.signature.receiver.clone();
                    std::iter::once(receiver)
                        .chain(arguments.iter().zip(&shaped.parameter_of).map(
                            |(argument, index)| {
                                let parameter = &shaped.candidate.signature.parameters[*index];
                                let is_lambda = matches!(argument.value, ArgumentValue::Lambda(_));
                                (!is_lambda).then(|| {
                                    parameter.ty.substitute(&shaped.candidate.substitution)
                                })
                            },
                        ))
                        .collect()
                })
                .collect();
        // A parameter is at least as specific as another when what it takes
        // fits the other — and anything fits a type parameter of the other
        // callee's own that its bounds allow.
        let at_least_as_specific = |first: usize, second: usize| {
            let second_parameters = &applicable[second].candidate.signature.type_parameters;
            parameter_types[first]
                .iter()
                .zip(&parameter_types[second])
                .all(
                    |(first_type, second_type)| match (first_type, second_type) {
                        (Some(first_type), Some(second_type)) => {
                            first_type == second_type
                                || self.types.fits(first_type, second_type) == Fit::Yes
                                || self.within_bounds(first_type, second_type, second_parameters)
                        }
                        _ => true,
                    },
                )
        };
        // Each ordered pair once.
        let count = applicable.len();
        let specific: Vec<Vec<bool>> = (0..count)
            .map(|first| {
                (0..count)
                    .map(|second| first != second && at_least_as_specific(first, second))
                    .collect()
            })
            .collect();
        let not_beaten: Vec<usize> = (0..count)
            .filter(|candidate| {
                !(0..count).any(|other| specific[other][*candidate] && !specific[*candidate][other])
            })
            .collect();
        let mut best: Vec<Shaped> = applicable
            .into_iter()
            .enumerate()
            .filter(|(index, _)| not_beaten.contains(index))
            .map(|(_, shaped)| shaped)
            .collect();
        let uses_vararg = |shaped: &Shaped| {
            shaped
                .parameter_of
                .iter()
                .any(|index| shaped.candidate.signature.parameters[*index].vararg)
        };
        if best.len() > 1 && best.iter().any(|shaped| !uses_vararg(shaped)) {
            best.retain(|shaped| !uses_vararg(shaped));
        }
        let defaults_used = |shaped: &Shaped| {
            shaped.candidate.signature.parameters.len() - dedup_count(&shaped.parameter_of)
        };
        if best.len() > 1 {
            let fewest = best.iter().map(defaults_used).min().unwrap_or(0);
            best.retain(|shaped| defaults_used(shaped) == fewest);
        }
        if best.len() > 1
            && best
                .iter()
                .any(|shaped| shaped.candidate.signature.type_parameters.is_empty())
        {
            best.retain(|shaped| shaped.candidate.signature.type_parameters.is_empty());
        }
        // Java's `f(int)` and `f(Integer)` both take `Int`: for an argument
        // that isn't null, the primitive one.
        let not_null_parameters = |shaped: &Shaped| {
            shaped
                .parameter_of
                .iter()
                .filter(|index| {
                    shaped.candidate.signature.parameters[**index]
                        .ty
                        .nullability()
                        == crate::Nullability::NotNull
                })
                .count()
        };
        if best.len() > 1 {
            let most = best.iter().map(not_null_parameters).max().unwrap_or(0);
            best.retain(|shaped| not_null_parameters(shaped) == most);
        }
        let mut unique: Vec<Shaped> = Vec::new();
        for shaped in best {
            let location = shaped.candidate.callable.location();
            if !unique
                .iter()
                .any(|kept| kept.candidate.callable.location() == location)
            {
                unique.push(shaped);
            }
        }
        unique
    }

    /// For an argument passed where the callee's own type parameter is
    /// expected: `No` when it can't satisfy the parameter's bounds
    /// (`<T extends Comparable<T>> f(T)` given a non-comparable value), else
    /// `Unknown`. `None` when the parameter isn't a callee type parameter.
    fn bounds_fit(&self, ty: &Ty, declared: &Ty, parameters: &[TypeParameterId]) -> Option<Fit> {
        let Ty::TypeParameter { parameter, .. } = declared else {
            return None;
        };
        if !parameters.contains(parameter) {
            return None;
        }
        let value = ty.clone().non_null();
        let violates = self
            .types
            .type_parameter_bounds(*parameter)
            .iter()
            .any(|bound| self.types.fits(&value, &bound.clone().non_null()) == Fit::No);
        Some(if violates { Fit::No } else { Fit::Unknown })
    }

    /// Whether `ty` is something the callee type parameter `parameter_type`
    /// (one of `parameters`) can stand for, by its bounds.
    fn within_bounds(&self, ty: &Ty, parameter_type: &Ty, parameters: &[TypeParameterId]) -> bool {
        let Ty::TypeParameter { parameter, .. } = parameter_type else {
            return false;
        };
        if !parameters.contains(parameter) {
            return false;
        }
        self.types
            .type_parameter_bounds(*parameter)
            .iter()
            .all(|bound| self.types.fits(ty, bound) != Fit::No)
    }

    fn arguments_fit(&self, shaped: &Shaped, arguments: &[Argument<'_>]) -> Fit {
        self.arguments_fit_with(shaped, arguments, &shaped.candidate.substitution)
    }

    /// How the (typed) arguments fit their parameters, the callee's type
    /// parameters bound by `substitution` (unbound ones fit anything).
    fn arguments_fit_with(
        &self,
        shaped: &Shaped,
        arguments: &[Argument<'_>],
        substitution: &Substitution,
    ) -> Fit {
        let mut fit = Fit::Yes;
        for (argument, index) in arguments.iter().zip(&shaped.parameter_of) {
            let parameter = &shaped.candidate.signature.parameters[*index];
            let declared = parameter.ty.substitute(substitution);
            let argument_fit = match &argument.value {
                _ if argument.spread => Fit::Unknown,
                ArgumentValue::Typed {
                    ty,
                    integer_literal,
                } => {
                    if *integer_literal && self.takes_integer_literal(&declared) {
                        Fit::Yes
                    } else if let Some(bounds_fit) =
                        self.bounds_fit(ty, &declared, &shaped.candidate.signature.type_parameters)
                    {
                        bounds_fit
                    } else {
                        self.types.fits(ty, &declared)
                    }
                }
                ArgumentValue::Lambda(node) => self.lambda_fits(*node, &declared),
                ArgumentValue::Pending(_) => Fit::Unknown,
            };
            fit = fit.and(argument_fit);
        }
        fit
    }

    /// Integer literals become `Long`, `Short` or `Byte` where one is
    /// expected.
    fn takes_integer_literal(&self, expected: &Ty) -> bool {
        let Some(class) = expected.class() else {
            return false;
        };
        ["Int", "Long", "Short", "Byte"].iter().any(|name| {
            matches!(self.types.builtin(name), Ty::Class(builtin) if builtin.class == class.class)
        })
    }

    /// Whether a lambda can be passed where `expected` is: a function type
    /// taking as many parameters as it declares (`it` counts as one or
    /// none), or something it may convert to.
    fn lambda_fits(&self, node: SyntaxNode<'_>, expected: &Ty) -> Fit {
        let function = match expected {
            Ty::Function(function) => (**function).clone(),
            Ty::Class(class) => match self.types.functional_interface_type(class) {
                Some(function) => function,
                None => return self.lambda_fits_class(class),
            },
            Ty::Nothing { .. } => return Fit::No,
            Ty::TypeParameter { .. } | Ty::Unknown => return Fit::Unknown,
        };
        let declared = declared_lambda_parameters(node);
        let fits = match declared {
            // `{ … }` takes no parameter, or one as `it`.
            None => function.parameters.len() <= 1,
            Some(count) => count == function.parameters.len(),
        };
        if fits { Fit::Yes } else { Fit::No }
    }

    /// A lambda where a class that isn't a functional interface is
    /// expected: only `Any` and the function types' own classes may take
    /// one.
    fn lambda_fits_class(&self, class: &crate::ty::ClassType) -> Fit {
        let tree = hir_def::item_tree(self.db, class.class.container);
        let name = tree.item(class.class.item).name;
        let is_kotlin = tree
            .package
            .segments()
            .first()
            .is_some_and(|first| first.as_str() == "kotlin");
        let may_be_function = is_kotlin
            && (name.as_str() == "Any"
                || name.as_str().starts_with("Function")
                || name.as_str().starts_with("KFunction"));
        if may_be_function {
            Fit::Unknown
        } else {
            Fit::No
        }
    }

    /// Infers the arguments not inferred yet, without expected types.
    fn type_pending(&mut self, arguments: &mut [Argument<'_>]) {
        for argument in arguments.iter_mut() {
            if let ArgumentValue::Pending(node) = argument.value {
                let ty = self.expression(node, None);
                argument.value = ArgumentValue::Typed {
                    ty,
                    integer_literal: is_integer_literal(node),
                };
            }
        }
    }

    /// Infers every argument not inferred yet, lambdas included.
    pub(super) fn type_remaining(&mut self, arguments: &mut [Argument<'_>]) {
        self.type_pending(arguments);
        for argument in arguments.iter_mut() {
            if let ArgumentValue::Lambda(node) = argument.value {
                let ty = self.function_value(node, None, None);
                argument.value = ArgumentValue::Typed {
                    ty,
                    integer_literal: false,
                };
            }
        }
    }

    /// Calls a value of function type: arguments inferred against its
    /// parameters (its receiver first, when it has one).
    fn invoke(&mut self, function: Option<FunctionType>, arguments: &mut [Argument<'_>]) -> Ty {
        let Some(function) = function else {
            self.type_remaining(arguments);
            return Ty::Unknown;
        };
        let parameters: Vec<Ty> = function
            .receiver
            .iter()
            .chain(function.parameters.iter())
            .cloned()
            .collect();
        let offset = parameters.len().saturating_sub(
            arguments
                .iter()
                .filter(|argument| argument.name.is_none())
                .count(),
        );
        for (index, argument) in arguments.iter_mut().enumerate() {
            let expected = parameters.get(index + offset).cloned();
            match argument.value {
                ArgumentValue::Pending(node) => {
                    let ty = self.expression(node, expected.as_ref());
                    argument.value = ArgumentValue::Typed {
                        ty,
                        integer_literal: false,
                    };
                }
                ArgumentValue::Lambda(node) => {
                    let ty = self.function_value(node, expected.as_ref(), None);
                    argument.value = ArgumentValue::Typed {
                        ty,
                        integer_literal: false,
                    };
                }
                ArgumentValue::Typed { .. } => {}
            }
        }
        function.result
    }

    /// A lambda or anonymous function argument.
    fn function_value(
        &mut self,
        node: SyntaxNode<'_>,
        expected: Option<&Ty>,
        label: Option<Name>,
    ) -> Ty {
        match node.kind() {
            "lambda_literal" => self.lambda(node, expected, label),
            _ => self.expression(node, expected),
        }
    }

    // --- arguments --------------------------------------------------------

    /// A call suffix's arguments (the trailing lambda last) and explicit
    /// type arguments.
    fn arguments_of<'tree>(&self, suffix: SyntaxNode<'tree>) -> (Vec<Argument<'tree>>, Vec<Ty>) {
        let mut arguments = Vec::new();
        let mut type_arguments = Vec::new();
        for part in named_children(suffix) {
            match part.kind() {
                "type_arguments" => {
                    for projection in named_children(part) {
                        let ty = self
                            .type_child(projection)
                            .map(|written| self.written_type(written))
                            .unwrap_or(Ty::Unknown);
                        type_arguments.push(ty);
                    }
                }
                "value_arguments" => {
                    for argument in
                        named_children(part).filter(|child| child.kind() == "value_argument")
                    {
                        if let Some(parsed) = self.argument(argument) {
                            arguments.push(parsed);
                        }
                    }
                }
                "annotated_lambda" => {
                    if let Some(lambda) =
                        named_children(part).find(|child| child.kind() == "lambda_literal")
                    {
                        arguments.push(Argument {
                            name: None,
                            label: None,
                            value: ArgumentValue::Lambda(lambda),
                            spread: false,
                            trailing: true,
                            written: Some(lambda),
                        });
                    }
                }
                _ => {}
            }
        }
        (arguments, type_arguments)
    }

    fn argument<'tree>(&self, node: SyntaxNode<'tree>) -> Option<Argument<'tree>> {
        let children: Vec<SyntaxNode<'tree>> = named_children(node).collect();
        let is_named = has_token(node, "=") && children.len() >= 2;
        let name = is_named.then(|| self.name(children[0]));
        let label = is_named.then(|| children[0]);
        let mut value = *children.last()?;
        let mut spread = false;
        if value.kind() == "spread_expression" {
            spread = true;
            value = named_children(value).last()?;
        }
        let lambda_node = match value.kind() {
            "lambda_literal" | "anonymous_function" => Some(value),
            "annotated_lambda" => {
                named_children(value).find(|child| child.kind() == "lambda_literal")
            }
            _ => None,
        };
        Some(Argument {
            name,
            label,
            value: match lambda_node {
                Some(lambda) => ArgumentValue::Lambda(lambda),
                None => ArgumentValue::Pending(value),
            },
            spread,
            trailing: false,
            written: Some(value),
        })
    }

    // --- callable references ----------------------------------------------

    /// `::name`, `Type::name`, `value::name`.
    pub(super) fn callable_reference(&mut self, node: SyntaxNode<'_>) -> Ty {
        let children: Vec<SyntaxNode<'_>> = named_children(node).collect();
        let Some(&name_node) = children
            .iter()
            .rev()
            .find(|child| child.kind() == "simple_identifier")
        else {
            return self.class_literal(node, &children);
        };
        let name = self.name(name_node);
        let receiver = children
            .iter()
            .find(|child| child.start_byte() < name_node.start_byte())
            .copied();
        let receiver_type = receiver.map(|receiver| match receiver.kind() {
            "type_identifier" | "user_type" | "nullable_type" => self.written_type(receiver),
            _ => self.expression(receiver, None),
        });
        let declarations: Vec<ItemLocation> = match &receiver_type {
            Some(receiver_type) => {
                let mut found: Vec<ItemLocation> = self
                    .types
                    .members_named(receiver_type, name)
                    .into_iter()
                    .map(|member| member.location)
                    .collect();
                if found.is_empty() {
                    found.extend(
                        self.property_of(receiver_type, name)
                            .map(|(location, _)| location),
                    );
                }
                if found.is_empty() {
                    let levels = self.receiver_levels(receiver_type);
                    found.extend(self.first_level_locations(&levels, name));
                }
                found
            }
            None => {
                if let Some(local) = self.lookup_local(name) {
                    let ty = self.local_type(local);
                    self.record(name_node, Reference::Local(local), ty.clone());
                    return ty;
                }
                let levels = self.simple_levels(name, name_node.start_byte());
                let mut found = self.first_level_locations(&levels, name);
                if found.is_empty()
                    && let Some((location, _)) = self.reference_property(name)
                {
                    found.push(location);
                }
                found
            }
        };
        match &declarations[..] {
            [] => {
                let site = match &receiver_type {
                    Some(receiver_type) => NameSite::Member(receiver_type.clone()),
                    None => NameSite::Scope,
                };
                self.record_unresolved(name_node, site);
                Ty::Unknown
            }
            [only] => {
                let ty = self.reference_type(*only, receiver_type.as_ref());
                self.record(name_node, Reference::Declaration(*only), ty.clone());
                ty
            }
            several => {
                self.record(
                    name_node,
                    Reference::Candidates(several.into()),
                    Ty::Unknown,
                );
                Ty::Unknown
            }
        }
    }

    /// `Type::class` (and `value::class`): a `KClass` of the type.
    fn class_literal(&mut self, node: SyntaxNode<'_>, children: &[SyntaxNode<'_>]) -> Ty {
        if !has_token(node, "class") {
            return Ty::Unknown;
        }
        let Some(&receiver) = children.first() else {
            return Ty::Unknown;
        };
        let class = match receiver.kind() {
            "type_identifier" | "user_type" | "nullable_type" => self.written_type(receiver),
            _ => self.expression(receiver, None).non_null(),
        };
        crate::lower::builtin(self.db, self.file, "kotlin.reflect", "KClass", vec![class])
    }

    /// A property a bare `::name` can mean: of an implicit receiver, or
    /// top-level.
    fn reference_property(&mut self, name: Name) -> Option<(ItemLocation, Ty)> {
        for index in (0..self.receivers.len()).rev() {
            let receiver = self.receiver_type(index);
            if let Some(found) = self.property_of(&receiver, name) {
                return Some(found);
            }
        }
        None
    }

    fn first_level_locations(&mut self, levels: &[Level], name: Name) -> Vec<ItemLocation> {
        for level in levels {
            let candidates = self.level_candidates(level, name);
            if !candidates.is_empty() {
                let mut locations: Vec<ItemLocation> = Vec::new();
                for candidate in candidates {
                    let location = candidate.callable.location();
                    if !locations.contains(&location) {
                        locations.push(location);
                    }
                }
                return locations;
            }
        }
        Vec::new()
    }

    /// The function type of a reference to a single declaration.
    fn reference_type(&self, location: ItemLocation, receiver: Option<&Ty>) -> Ty {
        if self.item_kind(location) != ItemKind::Function {
            return Ty::Unknown;
        }
        let Some(signature) = self.types.signature(Callable::Function(location)) else {
            return Ty::Unknown;
        };
        let erase_all = |ty: &Ty| erase(ty, &signature.type_parameters);
        Ty::Function(Box::new(FunctionType {
            receiver: receiver
                .cloned()
                .or(signature.receiver.as_ref().map(erase_all)),
            parameters: signature
                .parameters
                .iter()
                .map(|parameter| erase_all(&parameter.ty))
                .collect(),
            result: erase_all(&signature.result),
            suspend: false,
            nullability: crate::Nullability::NotNull,
        }))
    }
}

/// Matches arguments to `signature`'s parameters: positional ones in order
/// (a `vararg` takes all that follow), named ones by name, a trailing
/// lambda to the last. `None` when they don't match: too many, an unknown
/// name, a parameter given twice, or one without a default left out.
/// Whether a function takes the type arguments written (`f<T>(…)`): as
/// many as it declares, or none written. Kotlin doesn't call a function
/// with type arguments it has no parameters for.
fn takes_type_arguments(candidate: &Candidate, type_arguments: &[Ty]) -> bool {
    let is_function = matches!(candidate.callable, Callable::Function(_));
    type_arguments.is_empty()
        || !is_function
        || candidate.signature.type_parameters.len() == type_arguments.len()
}

fn match_arguments(
    signature: &CallableSignature,
    arguments: &[Argument<'_>],
) -> Option<Vec<usize>> {
    let parameters = &signature.parameters;
    let mut given = vec![false; parameters.len()];
    let mut parameter_of = Vec::with_capacity(arguments.len());
    let mut position = 0;
    for argument in arguments {
        let index = if argument.trailing {
            parameters.len().checked_sub(1)?
        } else if let Some(name) = argument.name {
            if signature.java {
                return None;
            }
            let index = parameters
                .iter()
                .position(|parameter| parameter.name == name)?;
            // A named argument in its own position counts as positional for
            // the ones after it (`f(a = 1, 2)`).
            if index == position && !parameters[index].vararg {
                position += 1;
            }
            index
        } else {
            let index = position;
            let parameter: &ParameterSignature = parameters.get(index)?;
            if !parameter.vararg {
                position += 1;
            }
            index
        };
        if given[index] && !parameters[index].vararg {
            return None;
        }
        given[index] = true;
        parameter_of.push(index);
    }
    let all_given = parameters
        .iter()
        .zip(&given)
        .all(|(parameter, given)| *given || parameter.has_default || parameter.vararg);
    all_given.then_some(parameter_of)
}

/// How many distinct parameters the arguments went to.
fn dedup_count(parameter_of: &[usize]) -> usize {
    let mut seen: Vec<usize> = parameter_of.to_vec();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// How many parameters a lambda declares; `None` when it declares none
/// (taking zero, or one as `it`).
fn declared_lambda_parameters(node: SyntaxNode<'_>) -> Option<usize> {
    if node.kind() != "lambda_literal" {
        return None;
    }
    named_children(node)
        .find(|child| child.kind() == "lambda_parameters")
        .map(|parameters| named_children(parameters).count())
}

fn is_integer_literal(node: SyntaxNode<'_>) -> bool {
    match node.kind() {
        "integer_literal" | "hex_literal" | "bin_literal" => true,
        "prefix_expression" => named_children(node).last().is_some_and(is_integer_literal),
        _ => false,
    }
}

/// `ty` with the type parameters in `parameters` — a callee's, not bound
/// by its call — made unknown.
pub(super) fn erase(ty: &Ty, parameters: &[TypeParameterId]) -> Ty {
    if parameters.is_empty() {
        return ty.clone();
    }
    let mut unknown = Substitution::default();
    for parameter in parameters {
        unknown.insert(*parameter, Ty::Unknown);
    }
    ty.substitute(&unknown)
}
