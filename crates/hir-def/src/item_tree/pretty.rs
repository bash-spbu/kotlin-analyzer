//! [`ItemTree`] as Kotlin-like text, one declaration per line; a Java
//! declaration's signature as Java.

use std::fmt::Write;

use super::{
    ClassKind, ImportTarget, Item, ItemId, ItemKind, ItemTree, Modifiers, Parameter, Signature,
    TypeArgument, TypeParameter, TypeRef, Variance, Visibility,
};

pub(super) fn print(tree: &ItemTree) -> String {
    let mut output = String::new();
    if !tree.package.is_root() {
        let _ = writeln!(output, "package {}", tree.package);
    }
    for import in tree.imports.iter() {
        match import.target {
            ImportTarget::Single { alias: Some(alias) } => {
                let _ = writeln!(output, "import {} as {alias}", import.path);
            }
            ImportTarget::Single { alias: None } => {
                let _ = writeln!(output, "import {}", import.path);
            }
            ImportTarget::Everything => {
                let _ = writeln!(output, "import {}.*", import.path);
            }
        }
    }
    for index in tree.top_level.iter() {
        print_item(tree, *index, 0, &mut output);
    }
    output
}

fn print_item(tree: &ItemTree, index: ItemId, depth: usize, output: &mut String) {
    let item = tree.item(index);
    output.push_str(&"  ".repeat(depth));
    output.push_str(&header(item));
    output.push('\n');
    for child in item.children.iter() {
        print_item(tree, *child, depth + 1, output);
    }
}

/// One declaration's line: modifiers, keyword, name, signature.
pub(crate) fn header(item: &Item) -> String {
    let mut words: Vec<String> = Vec::new();
    match item.visibility {
        Visibility::Public => {}
        Visibility::Protected => words.push("protected".to_owned()),
        Visibility::Internal => words.push("internal".to_owned()),
        Visibility::Private => words.push("private".to_owned()),
        Visibility::PackagePrivate => words.push("package-private".to_owned()),
    }
    let modifiers = format!("{:?}", item.modifiers);
    if !modifiers.is_empty() {
        words.push(modifiers);
    }
    let keyword = match item.kind {
        ItemKind::Class(ClassKind::Class) => "class",
        ItemKind::Class(ClassKind::Interface) => "interface",
        ItemKind::Class(ClassKind::Enum) => "enum class",
        ItemKind::Class(ClassKind::Annotation) => "annotation class",
        ItemKind::Class(ClassKind::Object) => "object",
        ItemKind::Class(ClassKind::CompanionObject) => "companion object",
        ItemKind::Function => "fun",
        ItemKind::Property { mutable: true } => "var",
        ItemKind::Property { mutable: false } => "val",
        ItemKind::TypeAlias => "typealias",
        ItemKind::EnumEntry => "entry",
        ItemKind::Constructor => "constructor",
    };
    words.push(keyword.to_owned());
    let mut declaration = String::new();
    if let Signature::Function {
        receiver: Some(receiver),
        ..
    }
    | Signature::Property {
        receiver: Some(receiver),
        ..
    } = &item.signature
    {
        let _ = write!(declaration, "{}.", type_text(receiver));
    }
    declaration.push_str(item.name.as_str());
    declaration.push_str(&type_parameters(&item.type_parameters));
    match &item.signature {
        Signature::Class {
            supertypes,
            primary_constructor,
        } => {
            if let Some(parameters) = primary_constructor {
                let _ = write!(declaration, "({})", parameter_list(parameters));
            }
            if !supertypes.is_empty() {
                let supertypes: Vec<String> = supertypes
                    .iter()
                    .map(|type_ref| type_text(type_ref))
                    .collect();
                let _ = write!(declaration, " : {}", supertypes.join(", "));
            }
        }
        Signature::Function {
            parameters,
            return_type,
            ..
        } => {
            let _ = write!(declaration, "({})", parameter_list(parameters));
            if let Some(return_type) = return_type {
                let _ = write!(declaration, ": {}", type_text(return_type));
            }
        }
        Signature::Property { type_ref, .. } => {
            if let Some(type_ref) = type_ref {
                let _ = write!(declaration, ": {}", type_text(type_ref));
            }
        }
        Signature::TypeAlias { target } => {
            let _ = write!(declaration, " = {}", type_text(target));
        }
        Signature::None => {}
    }
    words.push(declaration);
    words.join(" ")
}

fn type_parameters(parameters: &[TypeParameter]) -> String {
    if parameters.is_empty() {
        return String::new();
    }
    let parameters: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            let bounds: Vec<String> = parameter
                .bounds
                .iter()
                .map(|type_ref| type_text(type_ref))
                .collect();
            if bounds.is_empty() {
                parameter.name.to_string()
            } else {
                format!("{} : {}", parameter.name, bounds.join(" & "))
            }
        })
        .collect();
    format!("<{}>", parameters.join(", "))
}

fn parameter_list(parameters: &[Parameter]) -> String {
    let parameters: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            let mut text = String::new();
            if parameter.vararg {
                text.push_str("vararg ");
            }
            text.push_str(parameter.name.as_str());
            if let Some(type_ref) = &parameter.type_ref {
                let _ = write!(text, ": {}", type_text(type_ref));
            }
            if parameter.has_default {
                text.push_str(" = …");
            }
            text
        })
        .collect();
    parameters.join(", ")
}

/// A type reference as Kotlin text: `Map<String, List<out T>>?`.
pub fn type_text(type_ref: &TypeRef) -> String {
    match type_ref {
        TypeRef::Named {
            segments, nullable, ..
        } => {
            let segments: Vec<String> = segments
                .iter()
                .map(|segment| {
                    if segment.arguments.is_empty() {
                        return segment.name.to_string();
                    }
                    let arguments: Vec<String> = segment
                        .arguments
                        .iter()
                        .map(|argument| match argument {
                            TypeArgument::Star => "*".to_owned(),
                            TypeArgument::Type { variance, type_ref } => {
                                let prefix = match variance {
                                    Variance::Invariant => "",
                                    Variance::In => "in ",
                                    Variance::Out => "out ",
                                };
                                format!("{prefix}{}", type_text(type_ref))
                            }
                        })
                        .collect();
                    format!("{}<{}>", segment.name, arguments.join(", "))
                })
                .collect();
            format!("{}{}", segments.join("."), if *nullable { "?" } else { "" })
        }
        TypeRef::Function {
            receiver,
            parameters,
            result,
            suspend,
            nullable,
        } => {
            let parameters: Vec<String> = parameters
                .iter()
                .map(|type_ref| type_text(type_ref))
                .collect();
            let receiver = receiver
                .as_ref()
                .map(|receiver| format!("{}.", type_text(receiver)))
                .unwrap_or_default();
            let function = format!(
                "{}{receiver}({}) -> {}",
                if *suspend { "suspend " } else { "" },
                parameters.join(", "),
                type_text(result)
            );
            if *nullable {
                format!("({function})?")
            } else {
                function
            }
        }
        TypeRef::Qualified {
            package,
            classes,
            nullable,
        } => {
            let mut segments: Vec<String> = package
                .segments()
                .iter()
                .map(|segment| segment.to_string())
                .collect();
            for class in classes.iter() {
                let named = TypeRef::Named {
                    segments: Box::new([class.clone()]),
                    nullable: false,
                };
                segments.push(type_text(&named));
            }
            format!("{}{}", segments.join("."), if *nullable { "?" } else { "" })
        }
        TypeRef::TypeParameter { name, nullable } => {
            format!("{name}{}", if *nullable { "?" } else { "" })
        }
        TypeRef::Primitive { name } => name.to_string(),
        TypeRef::Array { element } => format!("{}[]", type_text(element)),
        TypeRef::Unreadable => "<?>".to_owned(),
    }
}

/// A Java declaration's signature as Java writes it.
pub(crate) fn java_header(
    tree: &ItemTree,
    index: ItemId,
    parameter_names: Option<&[String]>,
) -> String {
    let item = tree.item(index);
    let mut words: Vec<String> = Vec::new();
    match item.visibility {
        Visibility::Public | Visibility::Internal | Visibility::PackagePrivate => {}
        Visibility::Protected => words.push("protected".to_owned()),
        Visibility::Private => words.push("private".to_owned()),
    }
    for (flag, word) in [
        (Modifiers::STATIC, "static"),
        (Modifiers::ABSTRACT, "abstract"),
        (Modifiers::FINAL, "final"),
    ] {
        if item.modifiers.contains(flag) {
            words.push(word.to_owned());
        }
    }
    let type_parameters = java_type_parameters(&item.type_parameters);
    match (&item.kind, &item.signature) {
        (ItemKind::Class(kind), _) => {
            let keyword = match kind {
                ClassKind::Interface => "interface",
                ClassKind::Enum => "enum",
                ClassKind::Annotation => "@interface",
                ClassKind::Class | ClassKind::Object | ClassKind::CompanionObject => "class",
            };
            words.push(keyword.to_owned());
            words.push(format!("{}{type_parameters}", item.name));
        }
        (
            ItemKind::Function | ItemKind::Constructor,
            Signature::Function {
                parameters,
                return_type,
                ..
            },
        ) => {
            if !type_parameters.is_empty() {
                words.push(type_parameters);
            }
            let name = match (item.kind, item.parent) {
                (ItemKind::Constructor, Some(class)) => tree.item(class).name.to_string(),
                _ => {
                    let returned = return_type
                        .as_ref()
                        .map_or("void".to_owned(), |type_ref| java_type(type_ref));
                    words.push(returned);
                    item.name.to_string()
                }
            };
            words.push(format!(
                "{name}({})",
                java_parameters(parameters, parameter_names)
            ));
        }
        (ItemKind::Property { .. }, Signature::Property { type_ref, .. }) => {
            if let Some(type_ref) = type_ref {
                words.push(java_type(type_ref));
            }
            words.push(item.name.to_string());
        }
        _ => words.push(item.name.to_string()),
    }
    words.join(" ")
}

fn java_type_parameters(parameters: &[TypeParameter]) -> String {
    if parameters.is_empty() {
        return String::new();
    }
    let parameters: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            let bounds: Vec<String> = parameter
                .bounds
                .iter()
                .map(|type_ref| java_type(type_ref))
                .collect();
            if bounds.is_empty() {
                parameter.name.to_string()
            } else {
                format!("{} extends {}", parameter.name, bounds.join(" & "))
            }
        })
        .collect();
    format!("<{}>", parameters.join(", "))
}

/// `long id, String... names`: a vararg's array as `...`.
fn java_parameters(parameters: &[Parameter], names: Option<&[String]>) -> String {
    let names = names.filter(|names| names.len() == parameters.len());
    let parameters: Vec<String> = parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            let name = names.map_or(parameter.name.as_str(), |names| names[index].as_str());
            let written = parameter
                .type_ref
                .as_ref()
                .map_or("?".to_owned(), |type_ref| java_type(type_ref));
            let written = match (parameter.vararg, written.strip_suffix("[]")) {
                (true, Some(element)) => format!("{element}..."),
                (true, None) => format!("{written}..."),
                (false, _) => written,
            };
            format!("{written} {name}")
        })
        .collect();
    parameters.join(", ")
}

/// A type as Java writes it, by its simple name: `Map<String, ? extends
/// Number>`, `Map.Entry<K, V>`, `long[]`.
pub(crate) fn java_type(type_ref: &TypeRef) -> String {
    let arguments_text = |arguments: &[TypeArgument]| -> String {
        if arguments.is_empty() {
            return String::new();
        }
        let arguments: Vec<String> = arguments
            .iter()
            .map(|argument| match argument {
                TypeArgument::Star => "?".to_owned(),
                TypeArgument::Type { variance, type_ref } => match variance {
                    Variance::Invariant => java_type(type_ref),
                    Variance::Out => format!("? extends {}", java_type(type_ref)),
                    Variance::In => format!("? super {}", java_type(type_ref)),
                },
            })
            .collect();
        format!("<{}>", arguments.join(", "))
    };
    match type_ref {
        TypeRef::Named { segments, .. } => {
            // Leading lower-case segments are the package (`java.util`).
            let classes: Vec<String> = segments
                .iter()
                .skip_while(|segment| {
                    segment
                        .name
                        .as_str()
                        .starts_with(|char: char| char.is_ascii_lowercase())
                        && segments.len() > 1
                })
                .map(|segment| format!("{}{}", segment.name, arguments_text(&segment.arguments)))
                .collect();
            if classes.is_empty() {
                type_text(type_ref)
            } else {
                classes.join(".")
            }
        }
        TypeRef::Qualified { classes, .. } => classes
            .iter()
            .map(|class| format!("{}{}", class.name, arguments_text(&class.arguments)))
            .collect::<Vec<_>>()
            .join("."),
        TypeRef::TypeParameter { name, .. } => name.to_string(),
        TypeRef::Primitive { name } => name.to_string(),
        TypeRef::Array { element } => format!("{}[]", java_type(element)),
        TypeRef::Function { .. } | TypeRef::Unreadable => type_text(type_ref),
    }
}

#[cfg(test)]
mod tests {
    use intern::{Name, Path};

    use super::*;
    use crate::item_tree::TypeSegment;

    fn class(package: &str, names: &[&str], arguments: Vec<TypeArgument>) -> TypeRef {
        let last = names.len() - 1;
        TypeRef::Qualified {
            package: Path::parse(package),
            classes: names
                .iter()
                .enumerate()
                .map(|(index, name)| TypeSegment {
                    name: Name::new(name),
                    arguments: if index == last {
                        arguments.clone().into_boxed_slice()
                    } else {
                        Box::new([])
                    },
                })
                .collect(),
            nullable: true,
        }
    }

    #[test]
    fn a_class_files_types_read_as_java_with_simple_names() {
        let string = class("java.lang", &["String"], Vec::new());
        assert_eq!(java_type(&string), "String");
        let entry = class(
            "java.util",
            &["Map", "Entry"],
            vec![
                TypeArgument::Type {
                    variance: Variance::Invariant,
                    type_ref: string.clone().interned(),
                },
                TypeArgument::Type {
                    variance: Variance::Out,
                    type_ref: class("java.lang", &["Number"], Vec::new()).interned(),
                },
            ],
        );
        assert_eq!(java_type(&entry), "Map.Entry<String, ? extends Number>");
        let longs = TypeRef::Array {
            element: TypeRef::Primitive {
                name: Name::new("long"),
            }
            .interned(),
        };
        assert_eq!(java_type(&longs), "long[]");
        let wildcard = class("java.util", &["List"], vec![TypeArgument::Star]);
        assert_eq!(java_type(&wildcard), "List<?>");

        let names = vec!["first".to_owned(), "rest".to_owned()];
        let parameters = [
            Parameter {
                name: Name::new("p0"),
                type_ref: Some(
                    TypeRef::Primitive {
                        name: Name::new("long"),
                    }
                    .interned(),
                ),
                has_default: false,
                vararg: false,
            },
            Parameter {
                name: Name::new("p1"),
                type_ref: Some(
                    TypeRef::Array {
                        element: string.interned(),
                    }
                    .interned(),
                ),
                has_default: false,
                vararg: true,
            },
        ];
        assert_eq!(java_parameters(&parameters, None), "long p0, String... p1");
        assert_eq!(
            java_parameters(&parameters, Some(&names)),
            "long first, String... rest"
        );
        // Names that don't match the parameters are not used.
        assert_eq!(
            java_parameters(&parameters, Some(&names[..1])),
            "long p0, String... p1"
        );
    }
}
