//! Types: `Ty` with type arguments, nullability, function types and type
//! parameters; substitution; members of a type including inherited ones; the
//! Kotlin types that map to Java ones; inference of every expression in a body,
//! including overload resolution, lambdas and smart casts.

pub mod display;
pub mod infer;
pub mod lower;
pub mod members;
pub mod signature;
pub mod ty;

pub use display::display;
pub use lower::{TypeScope, lower_type};
pub use ty::{ClassType, FunctionType, Nullability, Substitution, Ty, TypeParameterId};

#[cfg(test)]
mod tests;
