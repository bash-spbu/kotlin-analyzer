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

/// A piece of a type's text: a class's name (with the class), or what
/// joins names (`<`, `, `, `?`, `) -> `).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TypePart {
    pub text: String,
    pub class: Option<ItemLocation>,
}

/// `ty`'s text in pieces, each class name apart with its class — for an
/// editor to link each name to its declaration.
pub fn display_parts(db: &dyn SourceDatabase, ty: &Ty) -> Vec<TypePart> {
    let mut parts = TypeParts::default();
    write(db, ty, &mut parts);
    parts.0
}

/// Where a type's text is written.
trait TypeText {
    fn text(&mut self, text: &str);
    /// The name of `class`, as written (`Map.Entry`).
    fn class(&mut self, name: &str, class: ItemLocation);
}

impl TypeText for String {
    fn text(&mut self, text: &str) {
        self.push_str(text);
    }

    fn class(&mut self, name: &str, _class: ItemLocation) {
        self.push_str(name);
    }
}

#[derive(Default)]
struct TypeParts(Vec<TypePart>);

impl TypeText for TypeParts {
    fn text(&mut self, text: &str) {
        match self.0.last_mut() {
            Some(last) if last.class.is_none() => last.text.push_str(text),
            _ => self.0.push(TypePart {
                text: text.to_owned(),
                class: None,
            }),
        }
    }

    fn class(&mut self, name: &str, class: ItemLocation) {
        self.0.push(TypePart {
            text: name.to_owned(),
            class: Some(class),
        });
    }
}

fn write(db: &dyn SourceDatabase, ty: &Ty, out: &mut impl TypeText) {
    match ty {
        Ty::Class(class) => {
            out.class(&class_name(db, class.class), class.class);
            if !class.arguments.is_empty() {
                out.text("<");
                for (position, argument) in class.arguments.iter().enumerate() {
                    if position > 0 {
                        out.text(", ");
                    }
                    match argument {
                        Ty::Unknown => out.text("*"),
                        known => write(db, known, out),
                    }
                }
                out.text(">");
            }
            write_nullability(class.nullability, out);
        }
        Ty::TypeParameter {
            parameter,
            nullability,
        } => {
            out.text(&type_parameter_name(db, *parameter));
            write_nullability(*nullability, out);
        }
        Ty::Function(function) => {
            let nullable = function.nullability == Nullability::Nullable;
            if nullable {
                out.text("(");
            }
            if function.suspend {
                out.text("suspend ");
            }
            if let Some(receiver) = &function.receiver {
                write(db, receiver, out);
                out.text(".");
            }
            out.text("(");
            for (position, parameter) in function.parameters.iter().enumerate() {
                if position > 0 {
                    out.text(", ");
                }
                write(db, parameter, out);
            }
            out.text(") -> ");
            write(db, &function.result, out);
            if nullable {
                out.text(")?");
            }
        }
        Ty::Nothing { nullable } => {
            out.text("Nothing");
            if *nullable {
                out.text("?");
            }
        }
        Ty::Unknown => out.text("{unknown}"),
        Ty::Error => out.text("{error}"),
    }
}

fn write_nullability(nullability: Nullability, out: &mut impl TypeText) {
    match nullability {
        Nullability::NotNull => {}
        Nullability::Nullable => out.text("?"),
        Nullability::Platform => out.text("!"),
    }
}
