//! A Java class file (one without Kotlin metadata) → [`LibraryClass`].

use intern::Name;

use crate::class_file::{ClassFile, Member as RawMember, access};
use crate::signature::{NestedNames, Parser};
use crate::{
    ClassContents, ClassKind, LibraryClass, LibraryType, Member, MemberKind, Modifiers, Origin,
    TypeParameter, ValueParameter, Visibility,
};

pub(crate) fn read(class: &ClassFile) -> ClassContents {
    if class.access & access::SYNTHETIC != 0 {
        return ClassContents::Nothing;
    }
    let names = NestedNames::new(&class.inner_classes);
    // A nested class's real access flags live in `InnerClasses`.
    let own_entry = class
        .inner_classes
        .iter()
        .find(|inner| inner.inner == class.name);
    let (flags, is_nested) = match own_entry {
        Some(entry) if entry.outer.is_none() || entry.simple_name.is_none() => {
            // Local or anonymous: not visible to anyone.
            return ClassContents::Nothing;
        }
        Some(entry) => (entry.access, true),
        None => (class.access, false),
    };
    let Some(visibility) = visibility_of(flags) else {
        return ClassContents::Nothing;
    };
    let kind = if flags & access::ANNOTATION != 0 {
        ClassKind::Annotation
    } else if flags & access::INTERFACE != 0 {
        ClassKind::Interface
    } else if flags & access::ENUM != 0 {
        ClassKind::Enum
    } else {
        ClassKind::Class
    };
    let is_interface = matches!(kind, ClassKind::Interface | ClassKind::Annotation);
    let (type_parameters, supertypes) = class_signature(class, &names);
    let mut enum_entries = Vec::new();
    let mut members = Vec::new();
    for field in &class.fields {
        if field.access & access::ENUM != 0 {
            enum_entries.push(Name::new(&field.name));
            continue;
        }
        if let Some(member) = read_field(field, &names) {
            members.push(member);
        }
    }
    let simple_name = names.class_name(&class.name).simple_name();
    let is_inner = is_nested && flags & access::STATIC == 0 && !is_interface;
    for method in &class.methods {
        if let Some(member) = read_method(method, &names, simple_name, is_inner) {
            members.push(member);
        }
    }
    ClassContents::Class(LibraryClass {
        name: names.class_name(&class.name),
        kind,
        origin: Origin::Java,
        visibility,
        modifiers: Modifiers {
            is_abstract: flags & access::ABSTRACT != 0 && !is_interface,
            is_open: flags & access::FINAL == 0 && !is_interface,
            is_inner,
            ..Modifiers::default()
        },
        deprecation: class.deprecation.clone().map(Box::new),
        annotations: names.class_names(&class.annotations),
        opt_in_marker: None,
        type_parameters,
        supertypes,
        members,
        enum_entries,
        sealed_subclasses: Vec::new(),
        source_file: class.source_file.clone(),
    })
}

fn visibility_of(flags: u16) -> Option<Visibility> {
    if flags & access::PUBLIC != 0 {
        Some(Visibility::Public)
    } else if flags & access::PROTECTED != 0 {
        Some(Visibility::Protected)
    } else {
        None
    }
}

fn class_signature(
    class: &ClassFile,
    names: &NestedNames,
) -> (Vec<TypeParameter>, Vec<LibraryType>) {
    if let Some(signature) = class
        .signature
        .as_deref()
        .and_then(|signature| Parser::new(signature, names).class_signature())
    {
        return (signature.type_parameters, signature.supertypes);
    }
    let supertypes = class
        .super_class
        .iter()
        .chain(class.interfaces.iter())
        .map(|binary| LibraryType::Class {
            name: names.class_name(binary),
            arguments: Vec::new(),
            nullable: false,
        })
        .collect();
    (Vec::new(), supertypes)
}

fn read_field(field: &RawMember, names: &NestedNames) -> Option<Member> {
    if field.access & access::SYNTHETIC != 0 {
        return None;
    }
    let visibility = visibility_of(field.access)?;
    let text = field.signature.as_deref().unwrap_or(&field.descriptor);
    let type_ref = Parser::new(text, names)
        .field_signature()
        .unwrap_or(LibraryType::Unreadable);
    Some(Member {
        contract: None,
        name: Name::new(&field.name),
        kind: MemberKind::Property {
            mutable: field.access & access::FINAL == 0,
        },
        visibility,
        modifiers: Modifiers {
            is_static: field.access & access::STATIC != 0,
            ..Modifiers::default()
        },
        deprecation: field.deprecation.clone().map(Box::new),
        annotations: names.class_names(&field.annotations),
        type_parameters: Vec::new(),
        receiver: None,
        parameters: Vec::new(),
        return_type: Some(type_ref),
    })
}

fn read_method(
    method: &RawMember,
    names: &NestedNames,
    class_name: Name,
    is_inner_class: bool,
) -> Option<Member> {
    let skipped = method.access & (access::SYNTHETIC | access::BRIDGE) != 0
        || method.name == "<clinit>"
        || method.name.contains('$');
    if skipped {
        return None;
    }
    let visibility = visibility_of(method.access)?;
    let is_constructor = method.name == "<init>";
    let from_signature = method
        .signature
        .as_deref()
        .and_then(|signature| Parser::new(signature, names).method_signature());
    // An inner class's constructor takes its outer instance first; the
    // descriptor shows it (and `-parameters` names it), a generic signature
    // doesn't.
    let mut parameter_names: &[String] = &method.parameter_names;
    let signature = match from_signature {
        Some(signature) => signature,
        None => {
            let mut signature = Parser::new(&method.descriptor, names).method_signature()?;
            let takes_outer_instance =
                is_constructor && is_inner_class && !signature.parameters.is_empty();
            if takes_outer_instance {
                signature.parameters.remove(0);
                if parameter_names.len() == signature.parameters.len() + 1 {
                    parameter_names = &parameter_names[1..];
                }
            }
            signature
        }
    };
    let is_varargs = method.access & access::VARARGS != 0;
    let count = signature.parameters.len();
    let parameters = signature
        .parameters
        .into_iter()
        .enumerate()
        .map(|(index, parameter_type)| {
            let vararg = is_varargs && index + 1 == count;
            let type_ref = match parameter_type {
                LibraryType::Array(element) if vararg => *element,
                other => other,
            };
            let name = parameter_names
                .get(index)
                .filter(|name| !name.is_empty())
                .map(|name| Name::new(name))
                .unwrap_or_else(|| Name::new(&format!("p{index}")));
            ValueParameter {
                name,
                type_ref,
                has_default: false,
                vararg,
            }
        })
        .collect();
    Some(Member {
        contract: None,
        name: if is_constructor {
            class_name
        } else {
            Name::new(&method.name)
        },
        kind: if is_constructor {
            MemberKind::Constructor
        } else {
            MemberKind::Function
        },
        visibility,
        modifiers: Modifiers {
            is_static: method.access & access::STATIC != 0,
            is_abstract: method.access & access::ABSTRACT != 0,
            ..Modifiers::default()
        },
        deprecation: method.deprecation.clone().map(Box::new),
        annotations: names.class_names(&method.annotations),
        type_parameters: signature.type_parameters,
        receiver: None,
        parameters,
        return_type: if is_constructor {
            None
        } else {
            Some(
                signature
                    .result
                    .unwrap_or(LibraryType::Primitive(Name::new("void"))),
            )
        },
    })
}
