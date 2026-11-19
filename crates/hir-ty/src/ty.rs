//! Types as the analysis computes with them.

use hir_def::ItemLocation;
use rustc_hash::FxHashMap;

/// A declared type parameter: the `index`-th of `owner`'s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeParameterId {
    pub owner: ItemLocation,
    pub index: u32,
}

/// Whether a type admits `null`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Nullability {
    NotNull,
    Nullable,
    /// A Java type: Kotlin doesn't know (`String!`).
    Platform,
}

impl Nullability {
    pub fn of(nullable: bool) -> Nullability {
        if nullable {
            Nullability::Nullable
        } else {
            Nullability::NotNull
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Ty {
    Class(ClassType),
    TypeParameter {
        parameter: TypeParameterId,
        nullability: Nullability,
    },
    Function(Box<FunctionType>),
    /// `Nothing` (`Nothing?` when nullable): the type of `return`, `throw`,
    /// `TODO()`, and of `null` itself.
    Nothing {
        nullable: bool,
    },
    /// What the analysis doesn't model or couldn't infer (overloads it
    /// can't tell apart, a type argument it can't follow, a star
    /// projection's): every check on it stays silent.
    Unknown,
    /// What the source names resolves to nothing: a type or a name nothing
    /// is found for, a type that can't be read, a declaration whose type
    /// depends on itself. Silent as `Unknown` — the miss is reported where
    /// it's found, if it's reported at all.
    Error,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ClassType {
    pub class: ItemLocation,
    /// `Unknown` for a star projection.
    pub arguments: Box<[Ty]>,
    pub nullability: Nullability,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FunctionType {
    pub receiver: Option<Ty>,
    pub parameters: Vec<Ty>,
    pub result: Ty,
    pub suspend: bool,
    pub nullability: Nullability,
}

impl Ty {
    /// Whether nothing is known of it: not inferred, or an error.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown | Ty::Error)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }

    /// Whether `parameter` appears anywhere in the type.
    pub fn mentions(&self, parameter: TypeParameterId) -> bool {
        match self {
            Ty::TypeParameter { parameter: own, .. } => *own == parameter,
            Ty::Class(class) => class
                .arguments
                .iter()
                .any(|argument| argument.mentions(parameter)),
            Ty::Function(function) => {
                let in_receiver = function
                    .receiver
                    .as_ref()
                    .is_some_and(|receiver| receiver.mentions(parameter));
                in_receiver
                    || function
                        .parameters
                        .iter()
                        .any(|value| value.mentions(parameter))
                    || function.result.mentions(parameter)
            }
            Ty::Nothing { .. } | Ty::Unknown | Ty::Error => false,
        }
    }

    /// Whether a type parameter appears anywhere in the type.
    pub fn has_type_parameter(&self) -> bool {
        match self {
            Ty::TypeParameter { .. } => true,
            Ty::Class(class) => class.arguments.iter().any(Ty::has_type_parameter),
            Ty::Function(function) => {
                let in_receiver = function
                    .receiver
                    .as_ref()
                    .is_some_and(Ty::has_type_parameter);
                in_receiver
                    || function.parameters.iter().any(Ty::has_type_parameter)
                    || function.result.has_type_parameter()
            }
            Ty::Nothing { .. } | Ty::Unknown | Ty::Error => false,
        }
    }

    /// Whether any part of the type is unknown (`Pair<String, *>`).
    pub fn has_unknown_part(&self) -> bool {
        match self {
            Ty::Unknown | Ty::Error => true,
            Ty::Class(class) => class.arguments.iter().any(Ty::has_unknown_part),
            Ty::Function(function) => {
                let receiver_unknown = function.receiver.as_ref().is_some_and(Ty::has_unknown_part);
                receiver_unknown
                    || function.parameters.iter().any(Ty::has_unknown_part)
                    || function.result.has_unknown_part()
            }
            Ty::TypeParameter { .. } | Ty::Nothing { .. } => false,
        }
    }

    /// Whether a value of this type can't be null: a class or function
    /// type without `?`. A type parameter may stand for a nullable type, a
    /// Java type's nullability isn't known, nor is an unknown type's.
    pub fn is_never_null(&self) -> bool {
        match self {
            Ty::Class(class) => class.nullability == Nullability::NotNull,
            Ty::Function(function) => function.nullability == Nullability::NotNull,
            Ty::TypeParameter { .. } | Ty::Nothing { .. } | Ty::Unknown | Ty::Error => false,
        }
    }

    pub fn nullability(&self) -> Nullability {
        match self {
            Ty::Class(class) => class.nullability,
            Ty::TypeParameter { nullability, .. } => *nullability,
            Ty::Function(function) => function.nullability,
            Ty::Nothing { nullable } => Nullability::of(*nullable),
            Ty::Unknown | Ty::Error => Nullability::Platform,
        }
    }

    /// The same type with `nullability`.
    pub fn with_nullability(self, nullability: Nullability) -> Ty {
        match self {
            Ty::Class(class) => Ty::Class(ClassType {
                nullability,
                ..class
            }),
            Ty::TypeParameter { parameter, .. } => Ty::TypeParameter {
                parameter,
                nullability,
            },
            Ty::Function(mut function) => {
                function.nullability = nullability;
                Ty::Function(function)
            }
            Ty::Nothing { .. } => Ty::Nothing {
                nullable: nullability == Nullability::Nullable,
            },
            Ty::Unknown | Ty::Error => self,
        }
    }

    /// `T` of `T?` / `T!`.
    pub fn non_null(self) -> Ty {
        self.with_nullability(Nullability::NotNull)
    }

    /// `X?` as `X`; a Java `X!` stays `X!` — nothing says it isn't null.
    pub fn without_question_mark(self) -> Ty {
        match self.nullability() {
            Nullability::Nullable => self.non_null(),
            Nullability::NotNull | Nullability::Platform => self,
        }
    }

    /// `T?` — unless already nullable or unknown.
    pub fn nullable(self) -> Ty {
        match self.nullability() {
            Nullability::NotNull | Nullability::Platform => {
                self.with_nullability(Nullability::Nullable)
            }
            Nullability::Nullable => self,
        }
    }

    pub fn class(&self) -> Option<&ClassType> {
        match self {
            Ty::Class(class) => Some(class),
            _ => None,
        }
    }

    /// Replaces type parameters by what `substitution` maps them to.
    pub fn substitute(&self, substitution: &Substitution) -> Ty {
        if substitution.is_empty() {
            return self.clone();
        }
        match self {
            Ty::TypeParameter {
                parameter,
                nullability,
            } => match substitution.get(*parameter) {
                Some(replacement) => {
                    let replaced = replacement.clone();
                    // A Java `T` is `T!`: with `T = String` it's `String!`,
                    // whose nullability Kotlin doesn't know either.
                    let keeps_platform = *nullability == Nullability::Platform
                        && replaced.nullability() == Nullability::NotNull;
                    match nullability {
                        Nullability::Nullable => replaced.nullable(),
                        _ if keeps_platform => replaced.with_nullability(Nullability::Platform),
                        _ => replaced,
                    }
                }
                None => self.clone(),
            },
            Ty::Class(class) => Ty::Class(ClassType {
                class: class.class,
                arguments: class
                    .arguments
                    .iter()
                    .map(|argument| argument.substitute(substitution))
                    .collect(),
                nullability: class.nullability,
            }),
            Ty::Function(function) => Ty::Function(Box::new(FunctionType {
                receiver: function
                    .receiver
                    .as_ref()
                    .map(|receiver| receiver.substitute(substitution)),
                parameters: function
                    .parameters
                    .iter()
                    .map(|parameter| parameter.substitute(substitution))
                    .collect(),
                result: function.result.substitute(substitution),
                suspend: function.suspend,
                nullability: function.nullability,
            })),
            Ty::Nothing { .. } | Ty::Unknown | Ty::Error => self.clone(),
        }
    }
}

/// Type parameters to the types they stand for.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Substitution {
    map: FxHashMap<TypeParameterId, Ty>,
}

impl Substitution {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, parameter: TypeParameterId) -> Option<&Ty> {
        self.map.get(&parameter)
    }

    pub fn insert(&mut self, parameter: TypeParameterId, ty: Ty) {
        self.map.insert(parameter, ty);
    }

    /// Records `parameter` = `ty` unless already known (first evidence
    /// wins); unknown evidence is no evidence.
    /// Binds `parameter` to `ty` unless it's bound already; a later binding
    /// can still make it nullable (`safeCall(fallback) { maybeNull }`
    /// binds `T` to `X` and `X?`: `T` is `X?`) or a Java type (`X!`).
    pub fn bind(&mut self, parameter: TypeParameterId, ty: Ty) {
        if ty.is_unknown() {
            return;
        }
        let Some(bound) = self.map.get_mut(&parameter) else {
            self.map.insert(parameter, ty);
            return;
        };
        let weaker = match (bound.nullability(), ty.nullability()) {
            (Nullability::NotNull | Nullability::Platform, Nullability::Nullable) => {
                Some(Nullability::Nullable)
            }
            (Nullability::NotNull, Nullability::Platform) => Some(Nullability::Platform),
            _ => None,
        };
        if let Some(nullability) = weaker {
            *bound = bound.clone().with_nullability(nullability);
        }
    }

    /// Leaves `parameter` unbound.
    pub fn forget(&mut self, parameter: TypeParameterId) {
        self.map.remove(&parameter);
    }

    /// Adds `other`'s bindings for parameters this one doesn't bind.
    pub fn extend(&mut self, other: &Substitution) {
        for (parameter, ty) in &other.map {
            self.map.entry(*parameter).or_insert_with(|| ty.clone());
        }
    }

    /// Each binding passed through `outer` (when a supertype's arguments
    /// are written in terms of the subtype's parameters).
    pub fn then(&self, outer: &Substitution) -> Substitution {
        Substitution {
            map: self
                .map
                .iter()
                .map(|(parameter, ty)| (*parameter, ty.substitute(outer)))
                .collect(),
        }
    }
}
