//! [`ItemTree`] as Kotlin-like text, one declaration per line.

use std::fmt::Write;

use super::{
    ClassKind, ImportTarget, Item, ItemId, ItemKind, ItemTree, Parameter, Signature, TypeArgument,
    TypeParameter, TypeRef, Variance, Visibility,
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
                let supertypes: Vec<String> = supertypes.iter().map(type_text).collect();
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
            let bounds: Vec<String> = parameter.bounds.iter().map(type_text).collect();
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
            let parameters: Vec<String> = parameters.iter().map(type_text).collect();
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
