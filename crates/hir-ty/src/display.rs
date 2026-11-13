//! Types as Kotlin writes them, with simple class names:
//! `Map.Entry<String, Int>?`, `T`, `String!`, `suspend R.(Int) -> Unit`.

use base_db::SourceDatabase;
use hir_def::{ItemLocation, item_tree};

use crate::ty::{Nullability, Ty, TypeParameterId};

pub fn display(db: &dyn SourceDatabase, ty: &Ty) -> String {
    let mut text = String::new();
    write(db, ty, &mut text);
    text
}

/// `Outer.Inner`: the class's name with the classes it is nested in.
pub fn class_name(db: &dyn SourceDatabase, class: ItemLocation) -> String {
    let tree = item_tree(db, class.container);
    let mut names = Vec::new();
    let mut current = Some(class.item);
    while let Some(index) = current {
        let item = tree.item(index);
        if !item.kind.is_classifier() {
            break;
        }
        names.push(item.name.as_str());
        current = item.parent;
    }
    names.reverse();
    names.join(".")
}

pub fn type_parameter_name(db: &dyn SourceDatabase, parameter: TypeParameterId) -> String {
    let tree = item_tree(db, parameter.owner.container);
    tree.item(parameter.owner.item)
        .type_parameters
        .get(parameter.index as usize)
        .map(|declared| declared.name.to_string())
        .unwrap_or_else(|| "?".to_owned())
}

fn write(db: &dyn SourceDatabase, ty: &Ty, text: &mut String) {
    match ty {
        Ty::Class(class) => {
            text.push_str(&class_name(db, class.class));
            if !class.arguments.is_empty() {
                text.push('<');
                for (position, argument) in class.arguments.iter().enumerate() {
                    if position > 0 {
                        text.push_str(", ");
                    }
                    match argument {
                        Ty::Unknown => text.push('*'),
                        known => write(db, known, text),
                    }
                }
                text.push('>');
            }
            write_nullability(class.nullability, text);
        }
        Ty::TypeParameter {
            parameter,
            nullability,
        } => {
            text.push_str(&type_parameter_name(db, *parameter));
            write_nullability(*nullability, text);
        }
        Ty::Function(function) => {
            let nullable = function.nullability == Nullability::Nullable;
            if nullable {
                text.push('(');
            }
            if function.suspend {
                text.push_str("suspend ");
            }
            if let Some(receiver) = &function.receiver {
                write(db, receiver, text);
                text.push('.');
            }
            text.push('(');
            for (position, parameter) in function.parameters.iter().enumerate() {
                if position > 0 {
                    text.push_str(", ");
                }
                write(db, parameter, text);
            }
            text.push_str(") -> ");
            write(db, &function.result, text);
            if nullable {
                text.push_str(")?");
            }
        }
        Ty::Nothing { nullable } => {
            text.push_str("Nothing");
            if *nullable {
                text.push('?');
            }
        }
        Ty::Unknown => text.push_str("{unknown}"),
    }
}

fn write_nullability(nullability: Nullability, text: &mut String) {
    match nullability {
        Nullability::NotNull => {}
        Nullability::Nullable => text.push('?'),
        Nullability::Platform => text.push('!'),
    }
}
