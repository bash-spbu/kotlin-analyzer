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
    /// Not known: every check on it stays silent.
    Unknown,
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
    pub fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }

    pub fn nullability(&self) -> Nullability {
        match self {
            Ty::Class(class) => class.nullability,
            Ty::TypeParameter { nullability, .. } => *nullability,
            Ty::Function(function) => function.nullability,
            Ty::Nothing { nullable } => Nullability::of(*nullable),
            Ty::Unknown => Nullability::Platform,
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
            Ty::Unknown => Ty::Unknown,
        }
    }

    /// `T` of `T?` / `T!`.
    pub fn non_null(self) -> Ty {
        self.with_nullability(Nullability::NotNull)
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
                    match nullability {
                        Nullability::Nullable => replaced.nullable(),
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
            Ty::Nothing { .. } | Ty::Unknown => self.clone(),
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
    pub fn bind(&mut self, parameter: TypeParameterId, ty: Ty) {
        if ty.is_unknown() {
            return;
        }
        self.map.entry(parameter).or_insert(ty);
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
