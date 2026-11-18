//! JVM type descriptors and generic signatures (JVM specification §4.3 and
//! §4.7.9.1) — `(Ljava/util/List<TT;>;I)V` — into [`LibraryType`]s.

use intern::{Name, Path};
use rustc_hash::FxHashMap;

use crate::{ClassName, LibraryType, Nesting, TypeArgument, TypeParameter, Variance};

/// Binary names of nested classes, `a/Outer$Inner` → (`a/Outer`, `Inner`),
/// from a class file's `InnerClasses` attribute — the only reliable way to
/// tell nesting from a `$` that is part of a name.
#[derive(Default)]
pub(crate) struct NestedNames {
    nested: FxHashMap<String, (String, String)>,
}

impl NestedNames {
    pub(crate) fn new(inner_classes: &[crate::class_file::InnerClass]) -> Self {
        let mut nested = FxHashMap::default();
        for inner in inner_classes {
            if let (Some(outer), Some(simple)) = (&inner.outer, &inner.simple_name) {
                nested.insert(inner.inner.clone(), (outer.clone(), simple.clone()));
            }
        }
        Self { nested }
    }

    /// The classes binary names name: an annotated declaration's
    /// annotations.
    pub(crate) fn class_names(&self, binaries: &[String]) -> Box<[ClassName]> {
        binaries
            .iter()
            .map(|binary| self.class_name(binary))
            .collect()
    }

    /// The class a binary name (`java/util/Map$Entry`) names.
    pub(crate) fn class_name(&self, binary: &str) -> ClassName {
        let mut classes = Vec::new();
        let mut current = binary.to_owned();
        let mut depth = 0;
        while let Some((outer, simple)) = self.nested.get(&current) {
            classes.push(Name::new(simple));
            current = outer.clone();
            depth += 1;
            if depth > 32 {
                break;
            }
        }
        let (package, outermost) = match current.rsplit_once('/') {
            Some((package, outermost)) => (package, outermost),
            None => ("", current.as_str()),
        };
        classes.push(Name::new(outermost));
        classes.reverse();
        ClassName {
            package: Path::new(
                package
                    .split('/')
                    .filter(|segment| !segment.is_empty())
                    .map(Name::new),
            ),
            classes: classes.into_boxed_slice(),
        }
    }
}

/// A method's or class's generic signature, parsed.
#[derive(Default)]
pub(crate) struct MethodSignature {
    pub(crate) type_parameters: Vec<TypeParameter>,
    pub(crate) parameters: Vec<LibraryType>,
    pub(crate) result: Option<LibraryType>,
}

#[derive(Default)]
pub(crate) struct ClassSignature {
    pub(crate) type_parameters: Vec<TypeParameter>,
    pub(crate) supertypes: Vec<LibraryType>,
}

pub(crate) struct Parser<'text, 'names> {
    text: &'text [u8],
    position: usize,
    names: &'names NestedNames,
    nesting: Nesting,
}

impl<'text, 'names> Parser<'text, 'names> {
    pub(crate) fn new(text: &'text str, names: &'names NestedNames) -> Self {
        Self {
            text: text.as_bytes(),
            position: 0,
            names,
            nesting: Nesting::TOP,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn expect(&mut self, byte: u8) -> Option<()> {
        (self.next()? == byte).then_some(())
    }

    fn identifier(&mut self, terminators: &[u8]) -> Option<&'text str> {
        let start = self.position;
        while let Some(byte) = self.peek() {
            if terminators.contains(&byte) {
                break;
            }
            self.position += 1;
        }
        std::str::from_utf8(&self.text[start..self.position]).ok()
    }

    pub(crate) fn class_signature(mut self) -> Option<ClassSignature> {
        let type_parameters = self.type_parameters()?;
        let mut supertypes = Vec::new();
        while self.peek().is_some() {
            supertypes.push(self.field_type()?);
        }
        Some(ClassSignature {
            type_parameters,
            supertypes,
        })
    }

    pub(crate) fn method_signature(mut self) -> Option<MethodSignature> {
        let type_parameters = self.type_parameters()?;
        self.expect(b'(')?;
        let mut parameters = Vec::new();
        while self.peek()? != b')' {
            parameters.push(self.type_signature()?);
        }
        self.expect(b')')?;
        let result = match self.peek()? {
            b'V' => {
                self.position += 1;
                None
            }
            _ => Some(self.type_signature()?),
        };
        Some(MethodSignature {
            type_parameters,
            parameters,
            result,
        })
    }

    pub(crate) fn field_signature(mut self) -> Option<LibraryType> {
        self.type_signature()
    }

    fn type_parameters(&mut self) -> Option<Vec<TypeParameter>> {
        let mut parameters = Vec::new();
        if self.peek() != Some(b'<') {
            return Some(parameters);
        }
        self.position += 1;
        while self.peek()? != b'>' {
            let name = Name::new(self.identifier(b":")?);
            let mut bounds = Vec::new();
            while self.peek() == Some(b':') {
                self.position += 1;
                // An empty class bound (`T::Ljava/lang/Comparable;`).
                if matches!(self.peek(), Some(b'L' | b'T' | b'[')) {
                    bounds.push(self.field_type()?);
                }
            }
            parameters.push(TypeParameter { name, bounds });
        }
        self.position += 1;
        Some(parameters)
    }

    /// Reads a type one level deeper: an array's element, a type argument.
    fn nested_type(&mut self, read: fn(&mut Self) -> Option<LibraryType>) -> Option<LibraryType> {
        let outer = self.nesting;
        self.nesting = outer.deeper()?;
        let type_ref = read(self);
        self.nesting = outer;
        type_ref
    }

    fn type_signature(&mut self) -> Option<LibraryType> {
        let primitive = match self.peek()? {
            b'B' => "byte",
            b'C' => "char",
            b'D' => "double",
            b'F' => "float",
            b'I' => "int",
            b'J' => "long",
            b'S' => "short",
            b'Z' => "boolean",
            _ => return self.field_type(),
        };
        self.position += 1;
        Some(LibraryType::Primitive(Name::new(primitive)))
    }

    fn field_type(&mut self) -> Option<LibraryType> {
        match self.next()? {
            b'L' => self.class_type(),
            b'T' => {
                let name = self.identifier(b";")?;
                self.expect(b';')?;
                Some(LibraryType::TypeParameter {
                    name: Name::new(name),
                    nullable: false,
                })
            }
            b'[' => Some(LibraryType::Array(Box::new(
                self.nested_type(Self::type_signature)?,
            ))),
            _ => None,
        }
    }

    /// After `L`: `pkg/Outer<A>.Inner<B>;`.
    fn class_type(&mut self) -> Option<LibraryType> {
        let mut binary = self.identifier(b"<.;")?.to_owned();
        let mut arguments = self.type_arguments()?;
        while self.peek() == Some(b'.') {
            self.position += 1;
            let inner = self.identifier(b"<.;")?;
            binary.push('$');
            binary.push_str(inner);
            arguments = self.type_arguments()?;
        }
        self.expect(b';')?;
        Some(LibraryType::Class {
            name: self.names.class_name(&binary),
            arguments,
            nullable: false,
        })
    }

    fn type_arguments(&mut self) -> Option<Vec<TypeArgument>> {
        let mut arguments = Vec::new();
        if self.peek() != Some(b'<') {
            return Some(arguments);
        }
        self.position += 1;
        while self.peek()? != b'>' {
            let argument = match self.peek()? {
                b'*' => {
                    self.position += 1;
                    TypeArgument::Star
                }
                b'+' => {
                    self.position += 1;
                    TypeArgument::Type {
                        variance: Variance::Out,
                        type_ref: self.nested_type(Self::field_type)?,
                    }
                }
                b'-' => {
                    self.position += 1;
                    TypeArgument::Type {
                        variance: Variance::In,
                        type_ref: self.nested_type(Self::field_type)?,
                    }
                }
                _ => TypeArgument::Type {
                    variance: Variance::Invariant,
                    type_ref: self.nested_type(Self::field_type)?,
                },
            };
            arguments.push(argument);
        }
        self.position += 1;
        Some(arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(type_ref: &LibraryType) -> String {
        match type_ref {
            LibraryType::Class {
                name, arguments, ..
            } => {
                if arguments.is_empty() {
                    return name.qualified();
                }
                let arguments: Vec<String> = arguments
                    .iter()
                    .map(|argument| match argument {
                        TypeArgument::Star => "*".to_owned(),
                        TypeArgument::Type { variance, type_ref } => {
                            let prefix = match variance {
                                Variance::Invariant => "",
                                Variance::In => "in ",
                                Variance::Out => "out ",
                            };
                            format!("{prefix}{}", render(type_ref))
                        }
                    })
                    .collect();
                format!("{}<{}>", name.qualified(), arguments.join(", "))
            }
            LibraryType::TypeParameter { name, .. } => name.to_string(),
            LibraryType::Primitive(name) => name.to_string(),
            LibraryType::Array(element) => format!("{}[]", render(element)),
            _ => "?".to_owned(),
        }
    }

    #[test]
    fn arrays_and_type_arguments_nested_past_the_limit_do_not_parse() {
        let names = NestedNames::new(&[]);
        let arrays = format!("{}I", "[".repeat(100_000));
        assert!(Parser::new(&arrays, &names).field_signature().is_none());
        let generics = format!("{}Ljava/lang/String;", "Ljava/util/List<".repeat(50_000));
        assert!(Parser::new(&generics, &names).field_signature().is_none());
        assert!(Parser::new("[[[I", &names).field_signature().is_some());
    }

    #[test]
    fn method_signatures_with_type_parameters_wildcards_and_arrays() {
        let names = NestedNames::default();
        let signature = Parser::new(
            "<T:Ljava/lang/Object;R::Ljava/lang/Comparable<-TR;>;>(Ljava/util/function/Function<-TT;+TR;>;[I)Ljava/util/List<TR;>;",
            &names,
        )
        .method_signature()
        .unwrap();
        let bounds: Vec<String> = signature.type_parameters[1]
            .bounds
            .iter()
            .map(render)
            .collect();
        assert_eq!(bounds, vec!["java.lang.Comparable<in R>"]);
        let parameters: Vec<String> = signature.parameters.iter().map(render).collect();
        assert_eq!(
            parameters,
            vec!["java.util.function.Function<in T, out R>", "int[]"]
        );
        assert_eq!(
            render(signature.result.as_ref().unwrap()),
            "java.util.List<R>"
        );
    }

    #[test]
    fn nested_classes_follow_inner_classes_and_keep_other_dollars() {
        let inner = vec![crate::class_file::InnerClass {
            inner: "java/util/Map$Entry".to_owned(),
            outer: Some("java/util/Map".to_owned()),
            simple_name: Some("Entry".to_owned()),
            access: 0,
        }];
        let names = NestedNames::new(&inner);
        assert_eq!(
            names.class_name("java/util/Map$Entry").qualified(),
            "java.util.Map.Entry"
        );
        assert_eq!(names.class_name("a/Odd$Name").qualified(), "a.Odd$Name");
        let void = Parser::new("()V", &names).method_signature().unwrap();
        assert!(void.result.is_none());
    }
}
