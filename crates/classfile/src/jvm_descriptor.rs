//! The JVM descriptor a Kotlin function or constructor compiles to, from the
//! types its metadata gives: `(Ljava/lang/Iterable;Lkotlin/jvm/functions/Function1;)I`
//! for `Iterable<T>.sumBy(selector: (T) -> Int): Int`. Metadata names a
//! member's JVM signature only when it isn't this one, so overloads are told
//! apart by it. `None` for what isn't mapped (value classes, whose methods
//! have mangled names): no descriptor rather than a wrong one.

use crate::{ClassName, LibraryType, Member, TypeArgument, TypeParameter};

/// `(receiver, parameters[, Continuation])result`.
pub(crate) fn of_function(function: &Member) -> Option<String> {
    let parameters = &function.type_parameters;
    let mut descriptor = String::from("(");
    if let Some(receiver) = &function.receiver {
        descriptor.push_str(&jvm_type(receiver, parameters, Position::Value)?);
    }
    for parameter in &function.parameters {
        descriptor.push_str(&parameter_type(
            &parameter.type_ref,
            parameter.vararg,
            parameters,
        )?);
    }
    if function.modifiers.is_suspend {
        descriptor.push_str("Lkotlin/coroutines/Continuation;)Ljava/lang/Object;");
        return Some(descriptor);
    }
    descriptor.push(')');
    let result = function.return_type.as_ref()?;
    descriptor.push_str(&jvm_type(result, parameters, Position::Result)?);
    Some(descriptor)
}

/// `(parameters)V`.
pub(crate) fn of_constructor(constructor: &Member) -> Option<String> {
    let mut descriptor = String::from("(");
    for parameter in &constructor.parameters {
        descriptor.push_str(&parameter_type(&parameter.type_ref, parameter.vararg, &[])?);
    }
    descriptor.push_str(")V");
    Some(descriptor)
}

/// Where a type stands: a value, an array's element, or a result (where
/// `Unit` is `void`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Value,
    ArrayElement,
    Result,
}

/// A `vararg`'s type is its element type: the parameter is an array of it.
fn parameter_type(
    type_ref: &LibraryType,
    vararg: bool,
    parameters: &[TypeParameter],
) -> Option<String> {
    if !vararg {
        return jvm_type(type_ref, parameters, Position::Value);
    }
    let element = match type_ref {
        // `vararg values: Int` is an `int[]`.
        LibraryType::Class {
            nullable: false, ..
        } => jvm_type(type_ref, parameters, Position::Value)?,
        _ => jvm_type(type_ref, parameters, Position::ArrayElement)?,
    };
    Some(format!("[{element}"))
}

fn jvm_type(
    type_ref: &LibraryType,
    parameters: &[TypeParameter],
    position: Position,
) -> Option<String> {
    match type_ref {
        LibraryType::Class {
            name,
            arguments,
            nullable,
        } => class_type(name, arguments, *nullable, parameters, position),
        // Erased to its first bound, else `Object`.
        LibraryType::TypeParameter { name, .. } => {
            match parameters.iter().find(|parameter| parameter.name == *name) {
                Some(parameter) => match parameter.bounds.first() {
                    Some(bound) => jvm_type(bound, &[], Position::ArrayElement),
                    None => Some("Ljava/lang/Object;".to_owned()),
                },
                // A class's type parameter: its bounds aren't known here,
                // so a bounded one maps wrongly — to no method, not to
                // another one.
                None => Some("Ljava/lang/Object;".to_owned()),
            }
        }
        LibraryType::Function {
            receiver,
            parameters: function_parameters,
            suspend,
            ..
        } => {
            let arity =
                function_parameters.len() + usize::from(receiver.is_some()) + usize::from(*suspend);
            Some(if arity <= 22 {
                format!("Lkotlin/jvm/functions/Function{arity};")
            } else {
                "Lkotlin/jvm/functions/FunctionN;".to_owned()
            })
        }
        LibraryType::Primitive(_) | LibraryType::Array(_) | LibraryType::Unreadable => None,
    }
}

fn class_type(
    name: &ClassName,
    arguments: &[TypeArgument],
    nullable: bool,
    parameters: &[TypeParameter],
    position: Position,
) -> Option<String> {
    let qualified = name.qualified();
    let qualified = qualified.as_str();
    if qualified == "kotlin.Unit" && position == Position::Result && !nullable {
        return Some("V".to_owned());
    }
    if let Some((primitive, boxed)) = primitive(qualified) {
        // A nullable one, or one inside `Array<…>`, is boxed.
        let boxes = nullable || position == Position::ArrayElement;
        return Some(if boxes {
            format!("L{boxed};")
        } else {
            primitive.to_owned()
        });
    }
    if let Some(element) = primitive_array(qualified) {
        return Some(format!("[{element}"));
    }
    if qualified == "kotlin.Array" {
        let element = match arguments.first() {
            Some(TypeArgument::Type { type_ref, .. }) => {
                jvm_type(type_ref, parameters, Position::ArrayElement)?
            }
            _ => "Ljava/lang/Object;".to_owned(),
        };
        return Some(format!("[{element}"));
    }
    if qualified == "kotlin.Nothing" {
        return Some("Ljava/lang/Void;".to_owned());
    }
    let internal = match mapped(qualified) {
        Some(java) => java.to_owned(),
        None => internal_name(name),
    };
    Some(format!("L{internal};"))
}

/// `kotlin.Int` → (`I`, `java/lang/Integer`).
fn primitive(qualified: &str) -> Option<(&'static str, &'static str)> {
    Some(match qualified {
        "kotlin.Int" => ("I", "java/lang/Integer"),
        "kotlin.Long" => ("J", "java/lang/Long"),
        "kotlin.Short" => ("S", "java/lang/Short"),
        "kotlin.Byte" => ("B", "java/lang/Byte"),
        "kotlin.Char" => ("C", "java/lang/Character"),
        "kotlin.Boolean" => ("Z", "java/lang/Boolean"),
        "kotlin.Float" => ("F", "java/lang/Float"),
        "kotlin.Double" => ("D", "java/lang/Double"),
        _ => return None,
    })
}

fn primitive_array(qualified: &str) -> Option<&'static str> {
    Some(match qualified {
        "kotlin.IntArray" => "I",
        "kotlin.LongArray" => "J",
        "kotlin.ShortArray" => "S",
        "kotlin.ByteArray" => "B",
        "kotlin.CharArray" => "C",
        "kotlin.BooleanArray" => "Z",
        "kotlin.FloatArray" => "F",
        "kotlin.DoubleArray" => "D",
        _ => return None,
    })
}

/// Kotlin types that are Java's on the JVM.
fn mapped(qualified: &str) -> Option<&'static str> {
    Some(match qualified {
        "kotlin.Any" => "java/lang/Object",
        "kotlin.String" => "java/lang/String",
        "kotlin.CharSequence" => "java/lang/CharSequence",
        "kotlin.Throwable" => "java/lang/Throwable",
        "kotlin.Cloneable" => "java/lang/Cloneable",
        "kotlin.Number" => "java/lang/Number",
        "kotlin.Comparable" => "java/lang/Comparable",
        "kotlin.Enum" => "java/lang/Enum",
        "kotlin.Annotation" => "java/lang/annotation/Annotation",
        "kotlin.collections.Iterable" | "kotlin.collections.MutableIterable" => {
            "java/lang/Iterable"
        }
        "kotlin.collections.Iterator" | "kotlin.collections.MutableIterator" => {
            "java/util/Iterator"
        }
        "kotlin.collections.Collection" | "kotlin.collections.MutableCollection" => {
            "java/util/Collection"
        }
        "kotlin.collections.List" | "kotlin.collections.MutableList" => "java/util/List",
        "kotlin.collections.ListIterator" | "kotlin.collections.MutableListIterator" => {
            "java/util/ListIterator"
        }
        "kotlin.collections.Set" | "kotlin.collections.MutableSet" => "java/util/Set",
        "kotlin.collections.Map" | "kotlin.collections.MutableMap" => "java/util/Map",
        "kotlin.collections.Map.Entry" | "kotlin.collections.MutableMap.MutableEntry" => {
            "java/util/Map$Entry"
        }
        _ => return None,
    })
}

/// `a.b` + `Outer`, `Inner` → `a/b/Outer$Inner`.
fn internal_name(name: &ClassName) -> String {
    let mut internal = name.package.to_string().replace('.', "/");
    if !internal.is_empty() {
        internal.push('/');
    }
    let classes: Vec<&str> = name.classes.iter().map(|class| class.as_str()).collect();
    internal.push_str(&classes.join("$"));
    internal
}
