//! A library's declarations as a source file, when its sources aren't to
//! be had: rendered from what its class files record — signatures,
//! modifiers, supertypes — with every body left out. A reading aid, as an
//! IDE shows a compiled class without sources; not a decompiler of method
//! bodies.
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

/// The file's text: its header, package, imports and `items` (top-level
/// items of `tree`, in one language), each with its members.
pub fn render(
    tree: &ItemTree,
    items: &[ItemId],
    language: StubLanguage,
    library_name: &str,
    missing: &MissingSources,
) -> String {
    let mut imports = Imports::new(&tree.package, language);
    for item in items {
        imports.collect_item(tree, *item);
    }
    let mut renderer = Renderer {
        tree,
        imports: &imports,
        language,
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
            StubLanguage::Java => "bodies are left out.",
        };
        let lines = [
            format!("Decompiled by kotlin-analyzer from {library_name}: {why}."),
            "Only the declarations are shown, as the class files record them;".to_owned(),
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
                self.java_class_body(item, *kind, depth);
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
                words.push(format!("{name}({})", self.java_parameters(parameters)));
                let body = if is_abstract {
                    ";".to_owned()
                } else {
                    format!(" {COMPILED_CODE}")
                };
                let _ = writeln!(self.text, "{}{body}", words.join(" "));
                return;
            }
            (ItemKind::Property { .. }, Signature::Property { type_ref, .. }) => {
                for (flag, word) in [(Modifiers::STATIC, "static"), (Modifiers::FINAL, "final")] {
                    // An interface's fields are static and final by being its.
                    if item.modifiers.contains(flag) && !in_interface {
                        words.push(word.to_owned());
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

    fn java_class_body(&mut self, item: &Item, kind: ClassKind, depth: usize) {
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

    fn java_parameters(&self, parameters: &[Parameter]) -> String {
        let parameters: Vec<String> = parameters
            .iter()
            .map(|parameter| {
                let written = parameter
                    .type_ref
                    .as_ref()
                    .map_or("Object".to_owned(), |type_ref| self.java_type(type_ref));
                let written = match (parameter.vararg, written.strip_suffix("[]")) {
                    (true, Some(element)) => format!("{element}..."),
                    (true, None) => format!("{written}..."),
                    (false, _) => written,
                };
                format!("{written} {}", parameter.name)
            })
            .collect();
        parameters.join(", ")
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
