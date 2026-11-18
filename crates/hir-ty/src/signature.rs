//! What calling a declaration takes and gives: its parameters' types, its
//! result, the type parameters a call infers.

use std::sync::Arc;

use base_db::{SourceDatabase, SourceSet};

use hir_def::expect_actual::{self, Side};
use hir_def::item_tree::{ItemKind, Parameter, Signature};
use hir_def::{ItemContainer, ItemLocation, item_tree};
use intern::Name;

use crate::infer::declaration_type;
use crate::lower::{TypeScope, lower_type};
use crate::members::Types;
use crate::ty::{Ty, TypeParameterId};

/// Something a call can name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Callable {
    /// A function — or a property of function type, called through
    /// `invoke`.
    Function(ItemLocation),
    /// A constructor of `class`: `declaration` is its own item, `None` for
    /// the primary constructor in the class header (or the implicit one).
    Constructor {
        class: ItemLocation,
        declaration: Option<ItemLocation>,
    },
    /// A function the compiler generates for a workspace class.
    Generated {
        class: ItemLocation,
        function: GeneratedFunction,
    },
}

/// What the compiler generates for data classes, enums and functional
/// interfaces.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GeneratedFunction {
    /// A data class's `copy(…)`: its constructor's parameters, each
    /// defaulting to the current value.
    Copy,
    /// An enum's `valueOf(name)`.
    ValueOf,
    /// An enum's `values()`.
    Values,
    /// `Name { … }` of a `fun interface` or a Java interface with one
    /// abstract method: takes that method as a function, gives the
    /// interface. Called by the interface's own name.
    SamConstructor,
}

impl GeneratedFunction {
    /// The name a call uses, for the functions named apart from their
    /// class (`None`: the SAM constructor, called by the class's name).
    pub fn name(self) -> Option<&'static str> {
        match self {
            GeneratedFunction::Copy => Some("copy"),
            GeneratedFunction::ValueOf => Some("valueOf"),
            GeneratedFunction::Values => Some("values"),
            GeneratedFunction::SamConstructor => None,
        }
    }
}

impl Callable {
    /// Where navigation goes: the constructor, or the class for its
    /// header's constructor.
    pub fn location(self) -> ItemLocation {
        match self {
            Callable::Function(location) => location,
            Callable::Constructor {
                class, declaration, ..
            } => declaration.unwrap_or(class),
            Callable::Generated { class, .. } => class,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CallableSignature {
    /// What a call infers: the function's own type parameters, or a
    /// constructor's class's.
    pub type_parameters: Vec<TypeParameterId>,
    pub receiver: Option<Ty>,
    pub parameters: Vec<ParameterSignature>,
    pub result: Ty,
    /// From Java: parameter names aren't part of the signature.
    pub java: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParameterSignature {
    pub name: Name,
    /// For a `vararg`, the element type.
    pub ty: Ty,
    pub has_default: bool,
    pub vararg: bool,
}

/// What a signature's result is when the declaration doesn't write it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnwrittenResult {
    /// Inferred from the body (`declaration_type`).
    Inferred,
    /// Left `Unknown` for the caller to infer: a shared signature mustn't
    /// depend on bodies, whose inference asks for signatures in turn.
    Deferred,
}

/// What calling `callable` takes and gives, as the files of `source_set`
/// see it — computed once for all of them (see `source_set_hierarchy`). A
/// workspace function's result it doesn't write is left to each file
/// (`UnwrittenResult::Deferred`).
#[salsa::tracked(returns(clone), lru = 16384)]
pub(crate) fn source_set_signature(
    db: &dyn SourceDatabase,
    source_set: SourceSet,
    callable: Callable,
) -> Option<Arc<CallableSignature>> {
    let file = *source_set.files(db).first()?;
    Types::new(db, file).compute_signature(callable, UnwrittenResult::Deferred)
}

impl Types<'_> {
    /// What calling `callable` takes and gives (remembered: a call site
    /// often weighs the same overloads as the last one).
    pub fn signature(&self, callable: Callable) -> Option<Arc<CallableSignature>> {
        if let Some(known) = self.signatures.borrow().get(&callable) {
            return known.clone();
        }
        let signature = match self.shared_by() {
            Some(source_set) => source_set_signature(self.db, source_set, callable)
                .map(|shared| self.with_inferred_result(callable, shared)),
            None => self.compute_signature(callable, UnwrittenResult::Inferred),
        };
        self.signatures
            .borrow_mut()
            .insert(callable, signature.clone());
        signature
    }

    /// A shared signature with the result its workspace function doesn't
    /// write, inferred.
    fn with_inferred_result(
        &self,
        callable: Callable,
        shared: Arc<CallableSignature>,
    ) -> Arc<CallableSignature> {
        let Callable::Function(location) = callable else {
            return shared;
        };
        if !result_is_inferred(self.db, location) {
            return shared;
        }
        Arc::new(CallableSignature {
            result: self.unwritten_type(location),
            ..(*shared).clone()
        })
    }

    fn compute_signature(
        &self,
        callable: Callable,
        unwritten: UnwrittenResult,
    ) -> Option<Arc<CallableSignature>> {
        match callable {
            Callable::Function(location) => self.function_signature(location, unwritten),
            Callable::Constructor { class, declaration } => {
                self.constructor_signature(class, declaration)
            }
            Callable::Generated { class, function } => self.generated_signature(class, function),
        }
        .map(|signature| self.with_expect_defaults(callable, signature))
        .map(Arc::new)
    }

    /// An `actual` function or constructor takes its parameters' default
    /// values from its `expect`: Kotlin writes them only there.
    fn with_expect_defaults(
        &self,
        callable: Callable,
        mut signature: CallableSignature,
    ) -> CallableSignature {
        let declared = match callable {
            Callable::Function(location) => location,
            Callable::Constructor { class, declaration } => declaration.unwrap_or(class),
            Callable::Generated { .. } => return signature,
        };
        if expect_actual::side_of(self.db, declared) != Side::Actual {
            return signature;
        }
        for counterpart in expect_actual::expect_actual_family(self.db, declared) {
            if expect_actual::side_of(self.db, counterpart) != Side::Expect {
                continue;
            }
            let expected = match callable {
                Callable::Constructor {
                    declaration: None, ..
                } => Callable::Constructor {
                    class: counterpart,
                    declaration: None,
                },
                Callable::Constructor { .. } => {
                    let tree = item_tree(self.db, counterpart.container);
                    let Some(parent) = tree.item(counterpart.item).parent else {
                        continue;
                    };
                    Callable::Constructor {
                        class: ItemLocation {
                            container: counterpart.container,
                            item: parent,
                        },
                        declaration: Some(counterpart),
                    }
                }
                _ => Callable::Function(counterpart),
            };
            let Some(expected_signature) = self.signature(expected) else {
                continue;
            };
            if expected_signature.parameters.len() != signature.parameters.len() {
                continue;
            }
            for (parameter, expected_parameter) in signature
                .parameters
                .iter_mut()
                .zip(expected_signature.parameters.iter())
            {
                parameter.has_default |= expected_parameter.has_default;
            }
            break;
        }
        signature
    }

    fn function_signature(
        &self,
        location: ItemLocation,
        unwritten: UnwrittenResult,
    ) -> Option<CallableSignature> {
        let tree = item_tree(self.db, location.container);
        let item = tree.item(location.item);
        let Signature::Function {
            receiver,
            parameters,
            return_type,
        } = &item.signature
        else {
            return None;
        };
        let scope = TypeScope::of_declaration(self.db, self.from, location);
        let deferred =
            unwritten == UnwrittenResult::Deferred && result_is_inferred(self.db, location);
        let result = match return_type {
            Some(written) => lower_type(self.db, scope, written),
            None if deferred => Ty::Unknown,
            None => self.unwritten_type(location),
        };
        Some(CallableSignature {
            type_parameters: own_type_parameters(location, item.type_parameters.len()),
            receiver: receiver
                .as_ref()
                .map(|receiver| lower_type(self.db, scope, receiver)),
            parameters: self.parameters(scope, parameters),
            result,
            java: crate::members::is_java(self.db, location),
        })
    }

    fn constructor_signature(
        &self,
        class: ItemLocation,
        declaration: Option<ItemLocation>,
    ) -> Option<CallableSignature> {
        let class_tree = item_tree(self.db, class.container);
        let class_item = class_tree.item(class.item);
        let parameters = match declaration {
            Some(constructor) => {
                let tree = item_tree(self.db, constructor.container);
                let Signature::Function { parameters, .. } = &tree.item(constructor.item).signature
                else {
                    return None;
                };
                let scope = TypeScope::of_declaration(self.db, self.from, constructor);
                self.parameters(scope, parameters)
            }
            None => match &class_item.signature {
                Signature::Class {
                    primary_constructor: Some(parameters),
                    ..
                } => {
                    let scope = TypeScope::of_declaration(self.db, self.from, class);
                    self.parameters(scope, parameters)
                }
                _ => Vec::new(),
            },
        };
        Some(CallableSignature {
            type_parameters: own_type_parameters(class, class_item.type_parameters.len()),
            receiver: None,
            parameters,
            result: Ty::Class(self.self_type(class)),
            java: crate::members::is_java(self.db, class),
        })
    }

    fn generated_signature(
        &self,
        class: ItemLocation,
        function: GeneratedFunction,
    ) -> Option<CallableSignature> {
        let class_type = Ty::Class(self.self_type(class));
        let (parameters, result) = match function {
            GeneratedFunction::Copy => {
                let constructor = self.constructor_signature(class, None)?;
                let parameters = constructor
                    .parameters
                    .into_iter()
                    .map(|parameter| ParameterSignature {
                        has_default: true,
                        ..parameter
                    })
                    .collect();
                (parameters, class_type)
            }
            GeneratedFunction::ValueOf => (
                vec![ParameterSignature {
                    name: Name::new("value"),
                    ty: self.builtin("String"),
                    has_default: false,
                    vararg: false,
                }],
                class_type,
            ),
            GeneratedFunction::Values => (
                Vec::new(),
                crate::lower::builtin(self.db, self.from, "kotlin", "Array", vec![class_type]),
            ),
            GeneratedFunction::SamConstructor => {
                let class_type = self.self_type(class);
                let function = self.functional_interface_type(&class_type)?;
                let class_item_tree = item_tree(self.db, class.container);
                let type_parameter_count = class_item_tree.item(class.item).type_parameters.len();
                return Some(CallableSignature {
                    type_parameters: own_type_parameters(class, type_parameter_count),
                    receiver: None,
                    parameters: vec![ParameterSignature {
                        name: Name::new("function"),
                        ty: Ty::Function(Box::new(function)),
                        has_default: false,
                        vararg: false,
                    }],
                    result: Ty::Class(class_type),
                    java: false,
                });
            }
        };
        Some(CallableSignature {
            type_parameters: Vec::new(),
            receiver: None,
            parameters,
            result,
            java: false,
        })
    }

    /// The functions the compiler generates named `name` for a workspace
    /// class: `copy` of a data class, `valueOf` and `values` of an enum.
    pub fn generated_functions(&self, class: ItemLocation, name: Name) -> Vec<Callable> {
        if !matches!(class.container, ItemContainer::File(_)) {
            return Vec::new();
        }
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        let candidates: &[GeneratedFunction] = match item.kind {
            ItemKind::Class(hir_def::item_tree::ClassKind::Enum) => {
                &[GeneratedFunction::ValueOf, GeneratedFunction::Values]
            }
            ItemKind::Class(_) if item.modifiers.contains(hir_def::item_tree::Modifiers::DATA) => {
                &[GeneratedFunction::Copy]
            }
            _ => &[],
        };
        candidates
            .iter()
            .filter(|function| function.name() == Some(name.as_str()))
            .map(|function| Callable::Generated {
                class,
                function: *function,
            })
            .collect()
    }

    /// The SAM constructor of `class`, when it's a Kotlin `fun interface`
    /// or a Java interface — with one abstract method.
    pub fn sam_constructor(&self, class: ItemLocation) -> Option<Callable> {
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        let is_interface = item.kind == ItemKind::Class(hir_def::item_tree::ClassKind::Interface);
        let may_have_one = item
            .modifiers
            .contains(hir_def::item_tree::Modifiers::FUN_INTERFACE)
            || crate::members::is_java(self.db, class);
        if !is_interface || !may_have_one {
            return None;
        }
        self.functional_interface_type(&self.self_type(class))?;
        Some(Callable::Generated {
            class,
            function: GeneratedFunction::SamConstructor,
        })
    }

    /// The constructors a call of the class's name can mean.
    pub fn constructors(&self, class: ItemLocation) -> Vec<Callable> {
        let tree = item_tree(self.db, class.container);
        let item = tree.item(class.item);
        if !matches!(
            item.kind,
            ItemKind::Class(
                hir_def::item_tree::ClassKind::Class | hir_def::item_tree::ClassKind::Enum
            )
        ) {
            return Vec::new();
        }
        let declared: Vec<Callable> = item
            .children
            .iter()
            .filter(|child| tree.item(**child).kind == ItemKind::Constructor)
            .map(|child| Callable::Constructor {
                class,
                declaration: Some(ItemLocation {
                    container: class.container,
                    item: *child,
                }),
            })
            .collect();
        let has_header_constructor = matches!(
            item.signature,
            Signature::Class {
                primary_constructor: Some(_),
                ..
            }
        );
        // A workspace class without constructors has the implicit one; a
        // compiled class lists every constructor it has.
        let implicit = declared.is_empty() && matches!(class.container, ItemContainer::File(_));
        let mut constructors = declared;
        if has_header_constructor || implicit {
            constructors.insert(
                0,
                Callable::Constructor {
                    class,
                    declaration: None,
                },
            );
        }
        constructors
    }

    /// The type of a property, field or enum entry.
    pub fn property_type(&self, location: ItemLocation) -> Ty {
        let tree = item_tree(self.db, location.container);
        let item = tree.item(location.item);
        match (&item.kind, &item.signature) {
            (ItemKind::EnumEntry, _) => match item.parent {
                Some(parent) => Ty::Class(self.self_type(ItemLocation {
                    container: location.container,
                    item: parent,
                })),
                None => Ty::Unknown,
            },
            (
                _,
                Signature::Property {
                    type_ref: Some(written),
                    ..
                },
            ) => lower_type(
                self.db,
                TypeScope::of_declaration(self.db, self.from, location),
                written,
            ),
            (_, Signature::Property { type_ref: None, .. }) => self.unwritten_type(location),
            _ => Ty::Unknown,
        }
    }

    /// The receiver type an extension property declares.
    pub fn property_receiver(&self, location: ItemLocation) -> Option<Ty> {
        let tree = item_tree(self.db, location.container);
        let Signature::Property {
            receiver: Some(receiver),
            ..
        } = &tree.item(location.item).signature
        else {
            return None;
        };
        Some(lower_type(
            self.db,
            TypeScope::of_declaration(self.db, self.from, location),
            receiver,
        ))
    }

    /// The type a declaration's body gives it when none is written: a
    /// compiled declaration without one returns `Unit`.
    fn unwritten_type(&self, location: ItemLocation) -> Ty {
        match location.container {
            ItemContainer::File(file) => declaration_type(self.db, file, location.item),
            ItemContainer::Library(_) => self.builtin("Unit"),
        }
    }

    fn parameters(&self, scope: TypeScope, parameters: &[Parameter]) -> Vec<ParameterSignature> {
        parameters
            .iter()
            .map(|parameter| ParameterSignature {
                name: parameter.name,
                ty: parameter
                    .type_ref
                    .as_ref()
                    .map(|written| lower_type(self.db, scope, written))
                    .unwrap_or(Ty::Unknown),
                has_default: parameter.has_default,
                vararg: parameter.vararg,
            })
            .collect()
    }
}

fn own_type_parameters(owner: ItemLocation, count: usize) -> Vec<TypeParameterId> {
    (0..count)
        .map(|index| TypeParameterId {
            owner,
            index: index as u32,
        })
        .collect()
}

/// A workspace function that doesn't write its result type: it's inferred
/// from the body.
fn result_is_inferred(db: &dyn SourceDatabase, location: ItemLocation) -> bool {
    if !matches!(location.container, ItemContainer::File(_)) {
        return false;
    }
    matches!(
        &item_tree(db, location.container)
            .item(location.item)
            .signature,
        Signature::Function {
            return_type: None,
            ..
        }
    )
}
