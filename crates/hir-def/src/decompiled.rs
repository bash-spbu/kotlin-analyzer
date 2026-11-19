//! A library's declarations as a source file, when its sources aren't to
//! be had: rendered from what its class files record — signatures,
//! modifiers, supertypes — and, in a Java class, the bodies `classfile`'s
//! decompiler reads back (those that run straight through); every other
//! body left out. A reading aid, as an IDE shows a compiled class without
//! sources, not the library's code.
//!
//! A Kotlin class's file is Kotlin (from its `@Metadata`), a Java class's
//! Java. Types are written by their simple names, imported, unless two
//! share a name. A header says where the text comes from, why, and that the
//! code stays its authors', under the library's license.

use std::collections::BTreeMap;
use std::fmt::Write;

use intern::{Name, Path};

use crate::ItemId;
use crate::item_tree::pretty::{java_type, type_text};
use crate::item_tree::{
    ClassKind, Item, ItemKind, ItemTree, Modifiers, Parameter, Signature, TypeArgument,
    TypeParameter, TypeRef, TypeSegment, Variance, Visibility,
};
use crate::resolver::{DEFAULT_IMPORT_PACKAGES, PLATFORM_DEFAULT_IMPORT_PACKAGES};
use classfile::decompile::{
    BinaryOperator, Body, DecompiledMethod, Expression, Label, Literal, LogicalOperator, LoopKind,
    Owner, Receiver, Relation, Statement, decompile_class, parameter_descriptors,
};
use classfile::{ClassName, LibraryType};
use rustc_hash::FxHashMap;

/// Why a declaration's text is rendered rather than read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MissingSources {
    /// The library comes without a sources archive.
    NoArchive,
    /// Its sources archive has no file declaring it.
    NotInArchive { archive_name: String },
}

/// The language a rendered file is in: its classes'.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StubLanguage {
    Kotlin,
    Java,
}

impl StubLanguage {
    /// Compiled from Java: what Kotlin sees as platform declarations.
    pub fn of(item: &Item) -> StubLanguage {
        if item.modifiers.contains(Modifiers::PLATFORM) {
            StubLanguage::Java
        } else {
            StubLanguage::Kotlin
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            StubLanguage::Kotlin => "kt",
            StubLanguage::Java => "java",
        }
    }
}

/// A method's body as its class file has it, decompiled.
pub struct MethodBody {
    body: Body,
    /// The method's parameters' names, from its local variables.
    parameter_names: Vec<String>,
}

/// The decompiled bodies of a file's methods and static initializers, by
/// item.
#[derive(Default)]
pub struct CompiledBodies {
    methods: FxHashMap<ItemId, MethodBody>,
    static_initializers: FxHashMap<ItemId, Body>,
}

impl CompiledBodies {
    /// The bodies of the Java classes among `items` and their nested
    /// classes: each class's file (by its binary name, `a/b/Outer$Inner`,
    /// from `read_class`) decompiled, its methods matched to the items by
    /// name and parameters.
    pub fn of(
        tree: &ItemTree,
        items: &[ItemId],
        read_class: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    ) -> CompiledBodies {
        let mut bodies = CompiledBodies::default();
        let package: Vec<String> = tree
            .package
            .segments()
            .iter()
            .map(|segment| segment.to_string())
            .collect();
        for item in items {
            let is_class = matches!(tree.item(*item).kind, ItemKind::Class(_));
            if is_class {
                let binary = package
                    .iter()
                    .cloned()
                    .chain([tree.item(*item).name.to_string()])
                    .collect::<Vec<_>>()
                    .join("/");
                bodies.read_class(tree, *item, &binary, read_class);
            }
        }
        bodies
    }

    fn read_class(
        &mut self,
        tree: &ItemTree,
        class: ItemId,
        binary: &str,
        read_class: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    ) {
        let item = tree.item(class);
        if StubLanguage::of(item) != StubLanguage::Java {
            return;
        }
        let decompiled = read_class(binary).and_then(|bytes| decompile_class(&bytes));
        if let Some(decompiled) = &decompiled {
            for child in item.children.iter() {
                if let Some(body) = matching_body(tree.item(*child), &decompiled.methods) {
                    self.methods.insert(*child, body);
                }
            }
            let static_initializer = decompiled
                .methods
                .iter()
                .find(|method| method.name == "<clinit>")
                .and_then(|method| method.body.as_ref().ok())
                .filter(|body| {
                    body.statements
                        .iter()
                        .any(|statement| *statement != Statement::Return(None))
                });
            if let Some(body) = static_initializer {
                self.static_initializers.insert(class, body.clone());
            }
        }
        for child in item.children.iter() {
            let nested = tree.item(*child);
            if matches!(nested.kind, ItemKind::Class(_)) {
                let nested_binary = format!("{binary}${}", nested.name);
                self.read_class(tree, *child, &nested_binary, read_class);
            }
        }
    }

    /// How many methods have a body: for the header.
    pub fn len(&self) -> usize {
        self.methods.len()
    }

    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }
}

/// The decompiled body of a method or constructor item: the class file's
/// method of its name whose parameters match — by count (a constructor of
/// an inner class takes its outer instance first), then, among overloads,
/// by their types' simple names.
fn matching_body(item: &Item, methods: &[DecompiledMethod]) -> Option<MethodBody> {
    let jvm_name = match item.kind {
        ItemKind::Constructor => "<init>".to_owned(),
        ItemKind::Function => item.name.to_string(),
        _ => return None,
    };
    let Signature::Function { parameters, .. } = &item.signature else {
        return None;
    };
    let wanted: Vec<String> = parameters
        .iter()
        .map(|parameter| {
            let simple = parameter
                .type_ref
                .as_ref()
                .map_or("*".to_owned(), simple_type_name);
            if parameter.vararg {
                format!("{simple}[]")
            } else {
                simple
            }
        })
        .collect();
    let candidates: Vec<(&DecompiledMethod, Vec<&str>)> = methods
        .iter()
        .filter(|method| method.name == jvm_name)
        .filter_map(|method| {
            let descriptors = parameter_descriptors(&method.descriptor)?;
            let takes_outer_instance =
                item.kind == ItemKind::Constructor && descriptors.len() == wanted.len() + 1;
            let own = if takes_outer_instance {
                descriptors[1..].to_vec()
            } else {
                descriptors
            };
            (own.len() == wanted.len()).then_some((method, own))
        })
        .collect();
    let chosen = match candidates.as_slice() {
        [only] => only,
        _ => candidates.iter().find(|(_, descriptors)| {
            descriptors.iter().zip(&wanted).all(|(descriptor, wanted)| {
                wanted == "*" || descriptor_simple_name(descriptor) == *wanted
            })
        })?,
    };
    let (method, own) = chosen;
    let body = method.body.as_ref().ok()?.clone();
    let skipped = method.parameter_names.len() - own.len();
    Some(MethodBody {
        body,
        parameter_names: method.parameter_names[skipped..].to_vec(),
    })
}

/// `int`, `String`, `Entry`, `long[]`: a type's simple name; `*` for a
/// type parameter, which matches anything.
fn simple_type_name(type_ref: &TypeRef) -> String {
    match type_ref {
        TypeRef::Qualified { classes, .. }
        | TypeRef::Named {
            segments: classes, ..
        } => classes
            .last()
            .map_or("*".to_owned(), |class| class.name.to_string()),
        TypeRef::Primitive { name } => name.to_string(),
        TypeRef::Array { element } => format!("{}[]", simple_type_name(element)),
        _ => "*".to_owned(),
    }
}

/// [`simple_type_name`] of a descriptor: `I` is `int`, `[Ljava/util/Map$Entry;`
/// is `Entry[]`.
fn descriptor_simple_name(descriptor: &str) -> String {
    if let Some(element) = descriptor.strip_prefix('[') {
        return format!("{}[]", descriptor_simple_name(element));
    }
    if let Some(class) = descriptor
        .strip_prefix('L')
        .and_then(|rest| rest.strip_suffix(';'))
    {
        let simple = class.rsplit('/').next().unwrap_or(class);
        return simple.rsplit('$').next().unwrap_or(simple).to_owned();
    }
    match descriptor {
        "Z" => "boolean",
        "B" => "byte",
        "C" => "char",
        "S" => "short",
        "I" => "int",
        "J" => "long",
        "F" => "float",
        "D" => "double",
        _ => "*",
    }
    .to_owned()
}

/// The file's text: its header, package, imports and `items` (top-level
/// items of `tree`, in one language), each with its members — and their
/// `bodies` where there are some.
pub fn render(
    tree: &ItemTree,
    items: &[ItemId],
    language: StubLanguage,
    library_name: &str,
    missing: &MissingSources,
    bodies: &CompiledBodies,
) -> String {
    let mut imports = Imports::new(&tree.package, language);
    for item in items {
        imports.collect_item(tree, *item);
    }
    for body in bodies
        .methods
        .values()
        .map(|method| &method.body)
        .chain(bodies.static_initializers.values())
    {
        imports.collect_body(body);
    }
    let mut renderer = Renderer {
        tree,
        imports: &imports,
        language,
        bodies,
        text: String::new(),
    };
    renderer.header(library_name, missing);
    let terminator = match language {
        StubLanguage::Kotlin => "",
        StubLanguage::Java => ";",
    };
    if !tree.package.is_root() {
        let _ = writeln!(renderer.text, "package {}{terminator}\n", tree.package);
    }
    let import_lines = imports.lines();
    if !import_lines.is_empty() {
        for import in &import_lines {
            let _ = writeln!(renderer.text, "import {import}{terminator}");
        }
        renderer.text.push('\n');
    }
    for (position, item) in items.iter().enumerate() {
        if position > 0 {
            renderer.text.push('\n');
        }
        renderer.item(*item, 0);
    }
    renderer.text
}

/// How a rendered body reads: compiled code isn't shown.
const COMPILED_CODE: &str = "{ /* compiled code */ }";

struct Renderer<'tree> {
    tree: &'tree ItemTree,
    imports: &'tree Imports,
    language: StubLanguage,
    bodies: &'tree CompiledBodies,
    text: String,
}

impl Renderer<'_> {
    fn header(&mut self, library_name: &str, missing: &MissingSources) {
        let why = match missing {
            MissingSources::NoArchive => "it comes without a sources jar".to_owned(),
            MissingSources::NotInArchive { archive_name } => {
                format!("its sources ({archive_name}) have no file declaring this")
            }
        };
        let left_out = match self.language {
            StubLanguage::Kotlin => {
                "bodies are left out, and parameters with defaults marked /* = … */."
            }
            StubLanguage::Java if self.bodies.is_empty() => "bodies are left out.",
            StubLanguage::Java => "bodies are decompiled where they can be, others left out.",
        };
        let shown = if self.bodies.is_empty() {
            "Only the declarations are shown, as the class files record them;"
        } else {
            "The declarations are shown as the class files record them;"
        };
        let lines = [
            format!("Decompiled by kotlin-analyzer from {library_name}: {why}."),
            shown.to_owned(),
            left_out.to_owned(),
            String::new(),
            "A reading aid, not the library's source: the code is its authors',".to_owned(),
            "under the library's own license. Check it before reusing anything here.".to_owned(),
        ];
        for line in lines {
            if line.is_empty() {
                self.text.push_str("//\n");
            } else {
                let _ = writeln!(self.text, "// {line}");
            }
        }
        self.text.push('\n');
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.text.push_str("    ");
        }
    }

    fn item(&mut self, index: ItemId, depth: usize) {
        match self.language {
            StubLanguage::Kotlin => self.kotlin_item(index, depth),
            StubLanguage::Java => self.java_item(index, depth),
        }
    }

    fn kotlin_type(&self, type_ref: &TypeRef) -> String {
        type_text(&self.imports.shortened(type_ref))
    }

    fn java_type(&self, type_ref: &TypeRef) -> String {
        java_type(&self.imports.shortened(type_ref))
    }

    fn kotlin_item(&mut self, index: ItemId, depth: usize) {
        let item = self.tree.item(index);
        self.indent(depth);
        let mut words = kotlin_visibility(item.visibility)
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>();
        words.extend(kotlin_modifiers(item).into_iter().map(str::to_owned));
        let receiver = match &item.signature {
            Signature::Function { receiver, .. } | Signature::Property { receiver, .. } => receiver
                .as_ref()
                .map(|receiver| format!("{}.", self.kotlin_type(receiver))),
            _ => None,
        }
        .unwrap_or_default();
        let type_parameters = self.kotlin_type_parameters(&item.type_parameters);
        match (&item.kind, &item.signature) {
            (ItemKind::Class(kind), signature) => {
                words.push(kotlin_class_keyword(*kind).to_owned());
                let mut declaration = format!("{}{type_parameters}", item.name);
                if let Signature::Class {
                    supertypes,
                    primary_constructor,
                } = signature
                {
                    if let Some(parameters) = primary_constructor {
                        let _ = write!(declaration, "({})", self.kotlin_parameters(parameters));
                    }
                    let supertypes: Vec<String> = supertypes
                        .iter()
                        .filter(|supertype| !is_implied_kotlin_supertype(supertype))
                        .map(|supertype| self.kotlin_type(supertype))
                        .collect();
                    if !supertypes.is_empty() {
                        let _ = write!(declaration, " : {}", supertypes.join(", "));
                    }
                }
                words.push(declaration);
                let _ = write!(self.text, "{}", words.join(" "));
                self.kotlin_class_body(item, *kind, depth);
                return;
            }
            (
                ItemKind::Function,
                Signature::Function {
                    parameters,
                    return_type,
                    ..
                },
            ) => {
                words.push("fun".to_owned());
                if !type_parameters.is_empty() {
                    words.push(type_parameters);
                }
                let mut declaration = format!(
                    "{receiver}{}({})",
                    item.name,
                    self.kotlin_parameters(parameters)
                );
                if let Some(return_type) = return_type {
                    let _ = write!(declaration, ": {}", self.kotlin_type(return_type));
                }
                words.push(declaration);
                if !item.modifiers.contains(Modifiers::ABSTRACT) {
                    words.push(COMPILED_CODE.to_owned());
                }
            }
            (ItemKind::Constructor, Signature::Function { parameters, .. }) => {
                words.push(format!(
                    "constructor({})",
                    self.kotlin_parameters(parameters)
                ));
            }
            (ItemKind::Property { mutable }, Signature::Property { type_ref, .. }) => {
                words.push(if *mutable { "var" } else { "val" }.to_owned());
                if !type_parameters.is_empty() {
                    words.push(type_parameters);
                }
                let mut declaration = format!("{receiver}{}", item.name);
                if let Some(type_ref) = type_ref {
                    let _ = write!(declaration, ": {}", self.kotlin_type(type_ref));
                }
                words.push(declaration);
            }
            (ItemKind::TypeAlias, Signature::TypeAlias { target }) => {
                words.push(format!(
                    "typealias {}{type_parameters} = {}",
                    item.name,
                    self.kotlin_type(target)
                ));
            }
            _ => words.push(item.name.to_string()),
        }
        let _ = writeln!(self.text, "{}", words.join(" "));
    }

    /// `{`, enum entries, members, `}` — or nothing for a class without
    /// any.
    fn kotlin_class_body(&mut self, item: &Item, kind: ClassKind, depth: usize) {
        let (entries, members) = entries_and_members(self.tree, item);
        if entries.is_empty() && members.is_empty() {
            self.text.push('\n');
            return;
        }
        self.text.push_str(" {\n");
        if kind == ClassKind::Enum {
            let names: Vec<String> = entries
                .iter()
                .map(|entry| self.tree.item(*entry).name.to_string())
                .collect();
            self.indent(depth + 1);
            let _ = writeln!(self.text, "{};", names.join(", "));
            if !members.is_empty() {
                self.text.push('\n');
            }
        }
        for member in members {
            self.kotlin_item(member, depth + 1);
        }
        self.indent(depth);
        self.text.push_str("}\n");
    }

    fn kotlin_type_parameters(&self, parameters: &[TypeParameter]) -> String {
        if parameters.is_empty() {
            return String::new();
        }
        let parameters: Vec<String> = parameters
            .iter()
            .map(|parameter| {
                let mut text = String::new();
                if parameter.reified {
                    text.push_str("reified ");
                }
                match parameter.variance {
                    Variance::Invariant => {}
                    Variance::In => text.push_str("in "),
                    Variance::Out => text.push_str("out "),
                }
                text.push_str(parameter.name.as_str());
                let bounds: Vec<String> = parameter
                    .bounds
                    .iter()
                    .map(|bound| self.kotlin_type(bound))
                    .filter(|bound| bound != "Any?")
                    .collect();
                if !bounds.is_empty() {
                    let _ = write!(text, " : {}", bounds.join(" & "));
                }
                text
            })
            .collect();
        format!("<{}>", parameters.join(", "))
    }

    fn kotlin_parameters(&self, parameters: &[Parameter]) -> String {
        let parameters: Vec<String> = parameters
            .iter()
            .map(|parameter| {
                let mut text = String::new();
                if parameter.vararg {
                    text.push_str("vararg ");
                }
                text.push_str(parameter.name.as_str());
                if let Some(type_ref) = &parameter.type_ref {
                    let _ = write!(text, ": {}", self.kotlin_type(type_ref));
                }
                if parameter.has_default {
                    text.push_str(" /* = … */");
                }
                text
            })
            .collect();
        parameters.join(", ")
    }

    fn java_item(&mut self, index: ItemId, depth: usize) {
        let item = self.tree.item(index);
        let parent_kind = item.parent.map(|parent| self.tree.item(parent).kind);
        let in_interface = matches!(
            parent_kind,
            Some(ItemKind::Class(
                ClassKind::Interface | ClassKind::Annotation
            ))
        );
        self.indent(depth);
        // An interface's members are public by being its.
        let visibility = java_visibility(item.visibility).filter(|_| !in_interface);
        let mut words: Vec<String> = visibility.map(str::to_owned).into_iter().collect();
        let type_parameters = self.java_type_parameters(&item.type_parameters);
        match (&item.kind, &item.signature) {
            (ItemKind::Class(kind), signature) => {
                words.extend(java_modifiers(item, *kind));
                words.push(java_class_keyword(*kind).to_owned());
                words.push(format!("{}{type_parameters}", item.name));
                if let Signature::Class { supertypes, .. } = signature {
                    words.extend(self.java_supertypes(*kind, supertypes));
                }
                let _ = writeln!(self.text, "{} {{", words.join(" "));
                self.java_class_body(index, *kind, depth);
                return;
            }
            (
                ItemKind::Function | ItemKind::Constructor,
                Signature::Function {
                    parameters,
                    return_type,
                    ..
                },
            ) => {
                let is_abstract = item.modifiers.contains(Modifiers::ABSTRACT);
                let is_static = item.modifiers.contains(Modifiers::STATIC);
                if is_static {
                    words.push("static".to_owned());
                }
                let is_default = in_interface && !is_abstract && !is_static;
                if is_default {
                    words.push("default".to_owned());
                }
                if is_abstract && !in_interface {
                    words.push("abstract".to_owned());
                }
                if item.modifiers.contains(Modifiers::FINAL) {
                    words.push("final".to_owned());
                }
                if !type_parameters.is_empty() {
                    words.push(type_parameters);
                }
                let name = match (item.kind, item.parent) {
                    (ItemKind::Constructor, Some(class)) => self.tree.item(class).name.to_string(),
                    _ => {
                        let returned = return_type
                            .as_ref()
                            .map_or("void".to_owned(), |type_ref| self.java_type(type_ref));
                        words.push(returned);
                        item.name.to_string()
                    }
                };
                let decompiled = self.bodies.methods.get(&index);
                let names = decompiled.map(|method| method.parameter_names.as_slice());
                words.push(format!(
                    "{name}({})",
                    self.java_parameters(parameters, names)
                ));
                let header = words.join(" ");
                match decompiled {
                    Some(method) => {
                        let _ = writeln!(self.text, "{header} {{");
                        self.java_statements(&method.body, depth + 1);
                        self.indent(depth);
                        self.text.push_str("}\n");
                    }
                    None if is_abstract => {
                        let _ = writeln!(self.text, "{header};");
                    }
                    None => {
                        let _ = writeln!(self.text, "{header} {COMPILED_CODE}");
                    }
                }
                return;
            }
            (ItemKind::Property { mutable }, Signature::Property { type_ref, .. }) => {
                // An interface's fields are static and final by being its.
                if !in_interface {
                    if item.modifiers.contains(Modifiers::STATIC) {
                        words.push("static".to_owned());
                    }
                    if !mutable {
                        words.push("final".to_owned());
                    }
                }
                let type_text = type_ref
                    .as_ref()
                    .map_or("Object".to_owned(), |type_ref| self.java_type(type_ref));
                words.push(type_text);
                words.push(item.name.to_string());
            }
            _ => words.push(item.name.to_string()),
        }
        let _ = writeln!(self.text, "{};", words.join(" "));
    }

    /// Enum entries, members, then the static initializer when decompiled.
    fn java_class_body(&mut self, class: ItemId, kind: ClassKind, depth: usize) {
        let item = self.tree.item(class);
        let (entries, members) = entries_and_members(self.tree, item);
        if kind == ClassKind::Enum {
            let names: Vec<String> = entries
                .iter()
                .map(|entry| self.tree.item(*entry).name.to_string())
                .collect();
            self.indent(depth + 1);
            let _ = writeln!(self.text, "{};", names.join(", "));
            if !members.is_empty() {
                self.text.push('\n');
            }
        }
        for member in members {
            self.java_item(member, depth + 1);
        }
        if let Some(body) = self.bodies.static_initializers.get(&class) {
            self.indent(depth + 1);
            self.text.push_str("static {\n");
            self.java_statements(body, depth + 2);
            self.indent(depth + 1);
            self.text.push_str("}\n");
        }
        self.indent(depth);
        self.text.push_str("}\n");
    }

    /// `extends Base implements A, B` (an interface's: `extends A, B`):
    /// the class file lists the superclass first; `Object`, `Enum` and
    /// `Annotation`, implied, left out.
    fn java_supertypes(&self, kind: ClassKind, supertypes: &[TypeRef]) -> Vec<String> {
        let is_interface = matches!(kind, ClassKind::Interface | ClassKind::Annotation);
        let mut superclass: Option<&TypeRef> = None;
        let mut interfaces: Vec<&TypeRef> = Vec::new();
        for (position, supertype) in supertypes.iter().enumerate() {
            if is_implied_java_supertype(supertype) {
                continue;
            }
            let is_superclass = position == 0 && !is_interface;
            if is_superclass {
                superclass = Some(supertype);
            } else {
                interfaces.push(supertype);
            }
        }
        let mut words = Vec::new();
        if let Some(superclass) = superclass {
            words.push(format!("extends {}", self.java_type(superclass)));
        }
        if !interfaces.is_empty() {
            let names: Vec<String> = interfaces
                .iter()
                .map(|interface| self.java_type(interface))
                .collect();
            let keyword = if is_interface {
                "extends"
            } else {
                "implements"
            };
            words.push(format!("{keyword} {}", names.join(", ")));
        }
        words
    }

    fn java_type_parameters(&self, parameters: &[TypeParameter]) -> String {
        if parameters.is_empty() {
            return String::new();
        }
        let parameters: Vec<String> = parameters
            .iter()
            .map(|parameter| {
                let bounds: Vec<String> = parameter
                    .bounds
                    .iter()
                    .filter(|bound| !is_implied_java_supertype(bound))
                    .map(|bound| self.java_type(bound))
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

    /// The parameters, named as `names` says when given (a decompiled
    /// body's, which uses them).
    fn java_parameters(&self, parameters: &[Parameter], names: Option<&[String]>) -> String {
        let names = names.filter(|names| names.len() == parameters.len());
        let parameters: Vec<String> = parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let written = parameter
                    .type_ref
                    .as_ref()
                    .map_or("Object".to_owned(), |type_ref| self.java_type(type_ref));
                let written = match (parameter.vararg, written.strip_suffix("[]")) {
                    (true, Some(element)) => format!("{element}..."),
                    (true, None) => format!("{written}..."),
                    (false, _) => written,
                };
                let name = names.map_or(parameter.name.as_str(), |names| names[index].as_str());
                format!("{written} {name}")
            })
            .collect();
        parameters.join(", ")
    }

    /// A body's statements, one per line; the `return;` ending a method
    /// and a constructor's `super()` left out, as Java leaves them.
    fn java_statements(&mut self, body: &Body, depth: usize) {
        let statements = match body.statements.split_last() {
            Some((Statement::Return(None), rest)) => rest,
            _ => body.statements.as_slice(),
        };
        self.java_block(statements, depth);
    }

    fn line(&mut self, depth: usize, text: &str) {
        self.indent(depth);
        self.text.push_str(text);
        self.text.push('\n');
    }

    fn java_block(&mut self, statements: &[Statement], depth: usize) {
        for statement in statements {
            self.java_statement(statement, depth);
        }
    }

    fn java_statement(&mut self, statement: &Statement, depth: usize) {
        let simple = match statement {
            Statement::SuperConstructor(arguments) if arguments.is_empty() => return,
            Statement::Expression(expression) => format!("{};", self.expression(expression)),
            Statement::Declare {
                type_ref,
                name,
                value,
            } => {
                let type_text = type_ref
                    .as_ref()
                    .map_or("var".to_owned(), |type_ref| self.library_type(type_ref));
                match value {
                    Some(value) => format!("{type_text} {name} = {};", self.expression(value)),
                    None => format!("{type_text} {name};"),
                }
            }
            Statement::Assign { target, value } => {
                format!("{} = {};", self.expression(target), self.expression(value))
            }
            Statement::Increment { name, by } => match by {
                1 => format!("{name}++;"),
                -1 => format!("{name}--;"),
                by if *by < 0 => format!("{name} -= {};", -i64::from(*by)),
                by => format!("{name} += {by};"),
            },
            Statement::Return(Some(value)) => format!("return {};", self.expression(value)),
            Statement::Return(None) => "return;".to_owned(),
            Statement::Throw(thrown) => format!("throw {};", self.expression(thrown)),
            Statement::SuperConstructor(arguments) => {
                format!("super({});", self.arguments(arguments))
            }
            Statement::ThisConstructor(arguments) => {
                format!("this({});", self.arguments(arguments))
            }
            Statement::Break(label) => format!("break{};", label_suffix(*label)),
            Statement::Continue(label) => format!("continue{};", label_suffix(*label)),
            Statement::If {
                condition,
                then,
                otherwise,
            } => {
                self.java_if(condition, then, otherwise, depth, "");
                return;
            }
            Statement::Loop { label, kind, body } => {
                let prefix =
                    label.map_or(String::new(), |label| format!("{}: ", label_name(label)));
                match kind {
                    LoopKind::DoWhile(condition) => {
                        self.line(depth, &format!("{prefix}do {{"));
                        self.java_block(body, depth + 1);
                        let condition = self.expression(condition);
                        self.line(depth, &format!("}} while ({condition});"));
                    }
                    LoopKind::Forever | LoopKind::While(_) => {
                        let condition = match kind {
                            LoopKind::While(condition) => self.expression(condition),
                            _ => "true".to_owned(),
                        };
                        self.line(depth, &format!("{prefix}while ({condition}) {{"));
                        self.java_block(body, depth + 1);
                        self.line(depth, "}");
                    }
                }
                return;
            }
            Statement::Block { label, body } => {
                self.line(depth, &format!("{}: {{", label_name(*label)));
                self.java_block(body, depth + 1);
                self.line(depth, "}");
                return;
            }
            Statement::Switch { value, cases } => {
                let value = self.expression(value);
                self.line(depth, &format!("switch ({value}) {{"));
                for case in cases {
                    let mut labels: Vec<String> = case
                        .values
                        .iter()
                        .map(|value| format!("case {value}:"))
                        .collect();
                    if case.default {
                        labels.push("default:".to_owned());
                    }
                    self.line(depth + 1, &labels.join(" "));
                    self.java_block(&case.body, depth + 2);
                }
                self.line(depth, "}");
                return;
            }
        };
        self.line(depth, &simple);
    }

    /// `if (…) { … } else if (…) { … } else { … }`.
    fn java_if(
        &mut self,
        condition: &Expression,
        then: &[Statement],
        otherwise: &[Statement],
        depth: usize,
        lead: &str,
    ) {
        let condition = self.expression(condition);
        if lead.is_empty() {
            self.line(depth, &format!("if ({condition}) {{"));
        } else {
            self.line(depth, &format!("{lead}if ({condition}) {{"));
        }
        self.java_block(then, depth + 1);
        match otherwise {
            [] => self.line(depth, "}"),
            [
                Statement::If {
                    condition,
                    then,
                    otherwise,
                },
            ] => self.java_if(condition, then, otherwise, depth, "} else "),
            _ => {
                self.line(depth, "} else {");
                self.java_block(otherwise, depth + 1);
                self.line(depth, "}");
            }
        }
    }

    fn library_type(&self, type_ref: &LibraryType) -> String {
        self.java_type(&crate::library::type_ref(type_ref))
    }

    fn class_name(&self, class: &ClassName) -> String {
        self.java_type(&class_type(class))
    }

    fn arguments(&self, arguments: &[Expression]) -> String {
        arguments
            .iter()
            .map(|argument| self.expression(argument))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// An expression, parenthesized where an operand binds looser than its
    /// place needs.
    fn operand(&self, expression: &Expression, at_least: u8) -> String {
        let text = self.expression(expression);
        if precedence(expression) < at_least {
            format!("({text})")
        } else {
            text
        }
    }

    fn expression(&self, expression: &Expression) -> String {
        match expression {
            Expression::This => "this".to_owned(),
            Expression::Local(name) => name.clone(),
            Expression::Literal(literal) => java_literal(literal),
            Expression::Field { owner, name } => match owner {
                Owner::Instance(object) => format!("{}.{name}", self.operand(object, PRIMARY)),
                Owner::Static(class) => format!("{}.{name}", self.class_name(class)),
            },
            Expression::Call {
                receiver,
                name,
                arguments,
            } => {
                let receiver = match receiver {
                    Receiver::Instance(object) => self.operand(object, PRIMARY),
                    Receiver::Static(class) => self.class_name(class),
                    Receiver::Super => "super".to_owned(),
                };
                format!("{receiver}.{name}({})", self.arguments(arguments))
            }
            Expression::New { class, arguments } => {
                format!(
                    "new {}({})",
                    self.class_name(class),
                    self.arguments(arguments)
                )
            }
            Expression::NewArray { element, length } => {
                let element = self.library_type(element);
                // `new int[3][]`, not `new int[][3]`: the length goes first.
                let (base, dimensions) = match element.find('[') {
                    Some(position) => element.split_at(position),
                    None => (element.as_str(), ""),
                };
                format!("new {base}[{}]{dimensions}", self.expression(length))
            }
            Expression::ArrayElement { array, index } => {
                format!(
                    "{}[{}]",
                    self.operand(array, PRIMARY),
                    self.expression(index)
                )
            }
            Expression::ArrayLength(array) => format!("{}.length", self.operand(array, PRIMARY)),
            Expression::Binary {
                operator,
                left,
                right,
            } => {
                let level = binary_precedence(*operator);
                format!(
                    "{} {} {}",
                    self.operand(left, level),
                    binary_symbol(*operator),
                    self.operand(right, level + 1)
                )
            }
            Expression::Negate(operand) => {
                let nested_minus = matches!(
                    operand.as_ref(),
                    Expression::Negate(_)
                        | Expression::Literal(
                            Literal::Int(i32::MIN..=-1) | Literal::Long(i64::MIN..=-1)
                        )
                );
                if nested_minus {
                    format!("-({})", self.expression(operand))
                } else {
                    format!("-{}", self.operand(operand, UNARY))
                }
            }
            Expression::Cast { to, operand } => {
                format!(
                    "({}) {}",
                    self.library_type(to),
                    self.operand(operand, UNARY)
                )
            }
            Expression::InstanceOf { operand, type_ref } => format!(
                "{} instanceof {}",
                self.operand(operand, RELATIONAL),
                self.library_type(type_ref)
            ),
            Expression::Concatenation(pieces) => {
                let is_text = |piece: Option<&Expression>| {
                    matches!(piece, Some(Expression::Literal(Literal::String(_))))
                };
                // Unless one of the first two is text, `+` would add them.
                let starts_as_text = is_text(pieces.first()) || is_text(pieces.get(1));
                let mut parts: Vec<String> = Vec::new();
                if !starts_as_text {
                    parts.push("\"\"".to_owned());
                }
                parts.extend(pieces.iter().map(|piece| self.operand(piece, ADDITIVE + 1)));
                parts.join(" + ")
            }
            Expression::ClassLiteral(type_ref) => format!("{}.class", self.library_type(type_ref)),
            Expression::Compare {
                relation,
                left,
                right,
                ..
            } => {
                let level = relation_precedence(*relation);
                format!(
                    "{} {} {}",
                    self.operand(left, level),
                    relation_symbol(*relation),
                    self.operand(right, level + 1)
                )
            }
            Expression::Not(operand) => format!("!{}", self.operand(operand, UNARY)),
            Expression::Logical {
                operator,
                left,
                right,
            } => {
                let (level, symbol) = match operator {
                    LogicalOperator::And => (AND, "&&"),
                    LogicalOperator::Or => (OR, "||"),
                };
                format!(
                    "{} {symbol} {}",
                    self.operand(left, level),
                    self.operand(right, level + 1)
                )
            }
            Expression::Conditional {
                condition,
                then,
                otherwise,
            } => format!(
                "{} ? {} : {}",
                self.operand(condition, OR),
                self.operand(then, CONDITIONAL + 1),
                self.operand(otherwise, CONDITIONAL)
            ),
            Expression::CompareResult { left, right, .. } => format!(
                "/* compare */ ({}, {})",
                self.expression(left),
                self.expression(right)
            ),
        }
    }
}

/// `label1` for a statement's label.
fn label_name(label: Label) -> String {
    format!("label{}", label.0)
}

fn label_suffix(label: Option<Label>) -> String {
    label.map_or(String::new(), |label| format!(" {}", label_name(label)))
}

fn relation_precedence(relation: Relation) -> u8 {
    match relation {
        Relation::Equal | Relation::NotEqual => EQUALITY,
        _ => RELATIONAL,
    }
}

fn relation_symbol(relation: Relation) -> &'static str {
    match relation {
        Relation::Equal => "==",
        Relation::NotEqual => "!=",
        Relation::Less => "<",
        Relation::LessOrEqual => "<=",
        Relation::Greater => ">",
        Relation::GreaterOrEqual => ">=",
    }
}

const PRIMARY: u8 = 16;
const UNARY: u8 = 14;
const CAST: u8 = 13;
const ADDITIVE: u8 = 11;
const RELATIONAL: u8 = 9;
const EQUALITY: u8 = 8;
const AND: u8 = 4;
const OR: u8 = 3;
const CONDITIONAL: u8 = 2;

/// How tightly an expression binds, as Java's grammar ranks it.
fn precedence(expression: &Expression) -> u8 {
    match expression {
        Expression::Binary { operator, .. } => binary_precedence(*operator),
        Expression::Concatenation(_) => ADDITIVE,
        Expression::InstanceOf { .. } => RELATIONAL,
        Expression::Compare { relation, .. } => relation_precedence(*relation),
        Expression::Not(_) => UNARY,
        Expression::Logical { operator, .. } => match operator {
            LogicalOperator::And => AND,
            LogicalOperator::Or => OR,
        },
        Expression::Conditional { .. } => CONDITIONAL,
        Expression::Cast { .. } => CAST,
        Expression::Negate(_) => UNARY,
        Expression::Literal(Literal::Int(i32::MIN..=-1) | Literal::Long(i64::MIN..=-1)) => UNARY,
        _ => PRIMARY,
    }
}

fn binary_precedence(operator: BinaryOperator) -> u8 {
    match operator {
        BinaryOperator::Multiply | BinaryOperator::Divide | BinaryOperator::Remainder => 12,
        BinaryOperator::Add | BinaryOperator::Subtract => ADDITIVE,
        BinaryOperator::ShiftLeft
        | BinaryOperator::ShiftRight
        | BinaryOperator::UnsignedShiftRight => 10,
        BinaryOperator::And => 7,
        BinaryOperator::Xor => 6,
        BinaryOperator::Or => 5,
    }
}

fn binary_symbol(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
        BinaryOperator::ShiftLeft => "<<",
        BinaryOperator::ShiftRight => ">>",
        BinaryOperator::UnsignedShiftRight => ">>>",
        BinaryOperator::And => "&",
        BinaryOperator::Or => "|",
        BinaryOperator::Xor => "^",
    }
}

/// A literal as Java writes it.
fn java_literal(literal: &Literal) -> String {
    match literal {
        Literal::Null => "null".to_owned(),
        Literal::Boolean(value) => value.to_string(),
        Literal::Int(value) => value.to_string(),
        Literal::Long(value) => format!("{value}L"),
        Literal::Float(value) if value.is_nan() => "Float.NaN".to_owned(),
        Literal::Float(value) if value.is_infinite() => {
            let sign = if *value > 0.0 { "POSITIVE" } else { "NEGATIVE" };
            format!("Float.{sign}_INFINITY")
        }
        Literal::Float(value) => format!("{value:?}F"),
        Literal::Double(value) if value.is_nan() => "Double.NaN".to_owned(),
        Literal::Double(value) if value.is_infinite() => {
            let sign = if *value > 0.0 { "POSITIVE" } else { "NEGATIVE" };
            format!("Double.{sign}_INFINITY")
        }
        Literal::Double(value) => format!("{value:?}"),
        Literal::Char(unit) => format!("'{}'", escaped_units(&[*unit], '\'')),
        Literal::String(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            format!("\"{}\"", escaped_units(&units, '"'))
        }
    }
}

/// UTF-16 units as Java source writes them inside `quote`s: escapes for
/// the quote, `\\`, controls and anything outside printable ASCII.
fn escaped_units(units: &[u16], quote: char) -> String {
    let mut text = String::new();
    for unit in units {
        match char::from_u32(u32::from(*unit)) {
            Some('\\') => text.push_str("\\\\"),
            Some('\n') => text.push_str("\\n"),
            Some('\t') => text.push_str("\\t"),
            Some('\r') => text.push_str("\\r"),
            Some(char) if char == quote => {
                text.push('\\');
                text.push(char);
            }
            Some(char) if (' '..='~').contains(&char) => text.push(char),
            _ => {
                let _ = write!(text, "\\u{unit:04x}");
            }
        }
    }
    text
}

/// A class as a type reference: no type arguments.
fn class_type(class: &ClassName) -> TypeRef {
    TypeRef::Qualified {
        package: class.package.clone(),
        classes: class
            .classes
            .iter()
            .map(|name| TypeSegment {
                name: *name,
                arguments: Box::new([]),
            })
            .collect(),
        nullable: false,
    }
}

/// A class's enum entries, then its other members as a reader looks for
/// them: constructors, properties, functions, nested classes.
fn entries_and_members(tree: &ItemTree, item: &Item) -> (Vec<ItemId>, Vec<ItemId>) {
    let (entries, mut members): (Vec<ItemId>, Vec<ItemId>) = item
        .children
        .iter()
        .partition(|child| tree.item(**child).kind == ItemKind::EnumEntry);
    members.sort_by_key(|member| match tree.item(*member).kind {
        ItemKind::Constructor => 0,
        ItemKind::Property { .. } => 1,
        ItemKind::Function => 2,
        ItemKind::TypeAlias => 3,
        ItemKind::Class(_) => 4,
        ItemKind::EnumEntry => 5,
    });
    (entries, members)
}

fn kotlin_visibility(visibility: Visibility) -> Option<&'static str> {
    match visibility {
        Visibility::Public | Visibility::PackagePrivate => None,
        Visibility::Protected => Some("protected"),
        Visibility::Internal => Some("internal"),
        Visibility::Private => Some("private"),
    }
}

fn java_visibility(visibility: Visibility) -> Option<&'static str> {
    match visibility {
        Visibility::Public | Visibility::Internal => Some("public"),
        Visibility::Protected => Some("protected"),
        Visibility::Private => Some("private"),
        Visibility::PackagePrivate => None,
    }
}

/// The Kotlin modifiers an item's declaration writes, in Kotlin's order.
fn kotlin_modifiers(item: &Item) -> Vec<&'static str> {
    let is_class = matches!(item.kind, ItemKind::Class(_));
    let written: &[(Modifiers, &str)] = if is_class {
        &[
            (Modifiers::ABSTRACT, "abstract"),
            (Modifiers::OPEN, "open"),
            (Modifiers::SEALED, "sealed"),
            (Modifiers::INNER, "inner"),
            (Modifiers::DATA, "data"),
            (Modifiers::VALUE, "value"),
            (Modifiers::FUN_INTERFACE, "fun"),
        ]
    } else {
        &[
            (Modifiers::OVERRIDE, "override"),
            (Modifiers::ABSTRACT, "abstract"),
            (Modifiers::OPEN, "open"),
            (Modifiers::CONST, "const"),
            (Modifiers::LATEINIT, "lateinit"),
            (Modifiers::SUSPEND, "suspend"),
            (Modifiers::INLINE, "inline"),
            (Modifiers::OPERATOR, "operator"),
            (Modifiers::INFIX, "infix"),
        ]
    };
    let is_interface = item.kind == ItemKind::Class(ClassKind::Interface);
    written
        .iter()
        .filter(|(flag, _)| item.modifiers.contains(*flag))
        // An interface is abstract by being one.
        .filter(|(flag, _)| !(is_interface && *flag == Modifiers::ABSTRACT))
        .map(|(_, word)| *word)
        .collect()
}

fn kotlin_class_keyword(kind: ClassKind) -> &'static str {
    match kind {
        ClassKind::Class => "class",
        ClassKind::Interface => "interface",
        ClassKind::Enum => "enum class",
        ClassKind::Annotation => "annotation class",
        ClassKind::Object => "object",
        ClassKind::CompanionObject => "companion object",
    }
}

fn java_class_keyword(kind: ClassKind) -> &'static str {
    match kind {
        ClassKind::Interface => "interface",
        ClassKind::Enum => "enum",
        ClassKind::Annotation => "@interface",
        ClassKind::Class | ClassKind::Object | ClassKind::CompanionObject => "class",
    }
}

fn java_modifiers(item: &Item, kind: ClassKind) -> Vec<String> {
    let mut words = Vec::new();
    if item.modifiers.contains(Modifiers::STATIC) {
        words.push("static".to_owned());
    }
    let is_plain_class = kind == ClassKind::Class;
    if is_plain_class && item.modifiers.contains(Modifiers::ABSTRACT) {
        words.push("abstract".to_owned());
    }
    if is_plain_class && item.modifiers.contains(Modifiers::FINAL) {
        words.push("final".to_owned());
    }
    words
}

/// The qualified name of a compiled class reference: `java.lang.Object`.
fn qualified_name(type_ref: &TypeRef) -> Option<String> {
    let TypeRef::Qualified {
        package, classes, ..
    } = type_ref
    else {
        return None;
    };
    let classes: Vec<&str> = classes.iter().map(|class| class.name.as_str()).collect();
    if package.is_root() {
        return Some(classes.join("."));
    }
    Some(format!("{package}.{}", classes.join(".")))
}

fn is_implied_kotlin_supertype(type_ref: &TypeRef) -> bool {
    matches!(
        qualified_name(type_ref).as_deref(),
        Some("kotlin.Any" | "java.lang.Object")
    )
}

fn is_implied_java_supertype(type_ref: &TypeRef) -> bool {
    matches!(
        qualified_name(type_ref).as_deref(),
        Some(
            "java.lang.Object"
                | "java.lang.Enum"
                | "java.lang.Record"
                | "java.lang.annotation.Annotation"
        )
    )
}

/// The imports a file needs: each class its types name, by its simple
/// name — but those of its own package and those every file sees
/// (`kotlin.*`, `java.lang`), and two of one name, which stay qualified.
struct Imports {
    package: Path,
    /// The packages every file sees.
    implicit: Vec<&'static str>,
    /// Outermost class's simple name → its qualified names.
    by_name: BTreeMap<String, Vec<String>>,
}

impl Imports {
    fn new(package: &Path, language: StubLanguage) -> Imports {
        let implicit: Vec<&'static str> = match language {
            StubLanguage::Kotlin => DEFAULT_IMPORT_PACKAGES
                .iter()
                .chain(PLATFORM_DEFAULT_IMPORT_PACKAGES)
                .copied()
                .collect(),
            StubLanguage::Java => vec!["java.lang"],
        };
        Imports {
            package: package.clone(),
            implicit,
            by_name: BTreeMap::new(),
        }
    }

    fn is_visible_without_import(&self, package: &Path) -> bool {
        let text = package.to_string();
        *package == self.package || self.implicit.contains(&text.as_str())
    }

    fn collect_item(&mut self, tree: &ItemTree, index: ItemId) {
        let item = tree.item(index);
        for parameter in item.type_parameters.iter() {
            for bound in parameter.bounds.iter() {
                self.collect(bound);
            }
        }
        match &item.signature {
            Signature::Class {
                supertypes,
                primary_constructor,
            } => {
                for supertype in supertypes.iter() {
                    self.collect(supertype);
                }
                for parameter in primary_constructor.iter().flatten() {
                    self.collect_parameter(parameter);
                }
            }
            Signature::Function {
                receiver,
                parameters,
                return_type,
            } => {
                receiver
                    .iter()
                    .chain(return_type.iter())
                    .for_each(|type_ref| self.collect(type_ref));
                for parameter in parameters.iter() {
                    self.collect_parameter(parameter);
                }
            }
            Signature::Property { receiver, type_ref } => {
                receiver
                    .iter()
                    .chain(type_ref.iter())
                    .for_each(|type_ref| self.collect(type_ref));
            }
            Signature::TypeAlias { target } => self.collect(target),
            Signature::None => {}
        }
        for child in item.children.iter() {
            self.collect_item(tree, *child);
        }
    }

    /// The classes a decompiled body names.
    fn collect_body(&mut self, body: &Body) {
        self.collect_statements(&body.statements);
    }

    fn collect_statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            match statement {
                Statement::Expression(expression)
                | Statement::Return(Some(expression))
                | Statement::Throw(expression) => self.collect_expression(expression),
                Statement::Declare {
                    type_ref, value, ..
                } => {
                    if let Some(type_ref) = type_ref {
                        self.collect(&crate::library::type_ref(type_ref));
                    }
                    if let Some(value) = value {
                        self.collect_expression(value);
                    }
                }
                Statement::Assign { target, value } => {
                    self.collect_expression(target);
                    self.collect_expression(value);
                }
                Statement::SuperConstructor(arguments) | Statement::ThisConstructor(arguments) => {
                    arguments
                        .iter()
                        .for_each(|argument| self.collect_expression(argument));
                }
                Statement::If {
                    condition,
                    then,
                    otherwise,
                } => {
                    self.collect_expression(condition);
                    self.collect_statements(then);
                    self.collect_statements(otherwise);
                }
                Statement::Loop { kind, body, .. } => {
                    if let LoopKind::While(condition) | LoopKind::DoWhile(condition) = kind {
                        self.collect_expression(condition);
                    }
                    self.collect_statements(body);
                }
                Statement::Block { body, .. } => self.collect_statements(body),
                Statement::Switch { value, cases } => {
                    self.collect_expression(value);
                    for case in cases {
                        self.collect_statements(&case.body);
                    }
                }
                Statement::Increment { .. }
                | Statement::Return(None)
                | Statement::Break(_)
                | Statement::Continue(_) => {}
            }
        }
    }

    fn collect_expression(&mut self, expression: &Expression) {
        match expression {
            Expression::This | Expression::Local(_) | Expression::Literal(_) => {}
            Expression::Field { owner, .. } => match owner {
                Owner::Instance(object) => self.collect_expression(object),
                Owner::Static(class) => self.collect(&class_type(class)),
            },
            Expression::Call {
                receiver,
                arguments,
                ..
            } => {
                match receiver {
                    Receiver::Instance(object) => self.collect_expression(object),
                    Receiver::Static(class) => self.collect(&class_type(class)),
                    Receiver::Super => {}
                }
                arguments
                    .iter()
                    .for_each(|argument| self.collect_expression(argument));
            }
            Expression::New { class, arguments } => {
                self.collect(&class_type(class));
                arguments
                    .iter()
                    .for_each(|argument| self.collect_expression(argument));
            }
            Expression::NewArray { element, length } => {
                self.collect(&crate::library::type_ref(element));
                self.collect_expression(length);
            }
            Expression::ArrayElement { array, index } => {
                self.collect_expression(array);
                self.collect_expression(index);
            }
            Expression::ArrayLength(operand) | Expression::Negate(operand) => {
                self.collect_expression(operand)
            }
            Expression::Binary { left, right, .. } => {
                self.collect_expression(left);
                self.collect_expression(right);
            }
            Expression::Cast { to, operand } => {
                self.collect(&crate::library::type_ref(to));
                self.collect_expression(operand);
            }
            Expression::InstanceOf { operand, type_ref } => {
                self.collect_expression(operand);
                self.collect(&crate::library::type_ref(type_ref));
            }
            Expression::Concatenation(pieces) => pieces
                .iter()
                .for_each(|piece| self.collect_expression(piece)),
            Expression::ClassLiteral(type_ref) => self.collect(&crate::library::type_ref(type_ref)),
            Expression::Not(operand) => self.collect_expression(operand),
            Expression::Compare { left, right, .. }
            | Expression::Logical { left, right, .. }
            | Expression::CompareResult { left, right, .. } => {
                self.collect_expression(left);
                self.collect_expression(right);
            }
            Expression::Conditional {
                condition,
                then,
                otherwise,
            } => {
                self.collect_expression(condition);
                self.collect_expression(then);
                self.collect_expression(otherwise);
            }
        }
    }

    fn collect_parameter(&mut self, parameter: &Parameter) {
        if let Some(type_ref) = &parameter.type_ref {
            self.collect(type_ref);
        }
    }

    fn collect(&mut self, type_ref: &TypeRef) {
        match type_ref {
            TypeRef::Qualified {
                package, classes, ..
            } => {
                if let Some(outermost) = classes.first()
                    && !self.is_visible_without_import(package)
                {
                    let qualified = if package.is_root() {
                        outermost.name.to_string()
                    } else {
                        format!("{package}.{}", outermost.name)
                    };
                    let known = self.by_name.entry(outermost.name.to_string()).or_default();
                    if !known.contains(&qualified) {
                        known.push(qualified);
                    }
                }
                for class in classes.iter() {
                    self.collect_arguments(&class.arguments);
                }
            }
            TypeRef::Named { segments, .. } => {
                for segment in segments.iter() {
                    self.collect_arguments(&segment.arguments);
                }
            }
            TypeRef::Function {
                receiver,
                parameters,
                result,
                ..
            } => {
                receiver.iter().for_each(|receiver| self.collect(receiver));
                parameters
                    .iter()
                    .for_each(|parameter| self.collect(parameter));
                self.collect(result);
            }
            TypeRef::Array { element } => self.collect(element),
            TypeRef::TypeParameter { .. } | TypeRef::Primitive { .. } | TypeRef::Unreadable => {}
        }
    }

    fn collect_arguments(&mut self, arguments: &[TypeArgument]) {
        for argument in arguments {
            if let TypeArgument::Type { type_ref, .. } = argument {
                self.collect(type_ref);
            }
        }
    }

    /// `import` lines' names, sorted: one per simple name only one class
    /// has.
    fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .by_name
            .values()
            .filter(|qualified| qualified.len() == 1)
            .map(|qualified| qualified[0].clone())
            .collect();
        lines.sort();
        lines
    }

    /// Whether a class may be written by its simple name.
    fn is_short(&self, package: &Path, outermost: &Name) -> bool {
        if self.is_visible_without_import(package) {
            return true;
        }
        self.by_name
            .get(outermost.as_str())
            .is_some_and(|qualified| qualified.len() == 1)
    }

    /// `type_ref` with each class that may be written by its simple name
    /// written so.
    fn shortened(&self, type_ref: &TypeRef) -> TypeRef {
        match type_ref {
            TypeRef::Qualified {
                package,
                classes,
                nullable,
            } => {
                let classes: Box<[TypeSegment]> = classes
                    .iter()
                    .map(|class| TypeSegment {
                        name: class.name,
                        arguments: self.shortened_arguments(&class.arguments),
                    })
                    .collect();
                let is_short = classes
                    .first()
                    .is_some_and(|outermost| self.is_short(package, &outermost.name));
                if is_short {
                    TypeRef::Named {
                        segments: classes,
                        nullable: *nullable,
                    }
                } else {
                    TypeRef::Qualified {
                        package: package.clone(),
                        classes,
                        nullable: *nullable,
                    }
                }
            }
            TypeRef::Named { segments, nullable } => TypeRef::Named {
                segments: segments
                    .iter()
                    .map(|segment| TypeSegment {
                        name: segment.name,
                        arguments: self.shortened_arguments(&segment.arguments),
                    })
                    .collect(),
                nullable: *nullable,
            },
            TypeRef::Function {
                receiver,
                parameters,
                result,
                suspend,
                nullable,
            } => TypeRef::Function {
                receiver: receiver
                    .as_ref()
                    .map(|receiver| Box::new(self.shortened(receiver))),
                parameters: parameters
                    .iter()
                    .map(|parameter| self.shortened(parameter))
                    .collect(),
                result: Box::new(self.shortened(result)),
                suspend: *suspend,
                nullable: *nullable,
            },
            TypeRef::Array { element } => TypeRef::Array {
                element: Box::new(self.shortened(element)),
            },
            other => other.clone(),
        }
    }

    fn shortened_arguments(&self, arguments: &[TypeArgument]) -> Box<[TypeArgument]> {
        arguments
            .iter()
            .map(|argument| match argument {
                TypeArgument::Star => TypeArgument::Star,
                TypeArgument::Type { variance, type_ref } => TypeArgument::Type {
                    variance: *variance,
                    type_ref: self.shortened(type_ref),
                },
            })
            .collect()
    }
}
