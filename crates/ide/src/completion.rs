//! `textDocument/completion`: what can be written at the cursor — after a
//! `.`, the receiver's members and the extensions it can use; on its own,
//! the locals, the implicit receivers' members and the declarations in
//! scope; in a type, the classes; plus, by name, declarations not imported
//! yet (with the import to add).

use std::sync::Arc;

use hir::{
    CompletionScope, FileId, ItemKind, ItemLocation, Name, Path, Qualifier, RootDatabase,
    Signature, SourceDatabase, SourceFile, Ty, Types,
};
use rustc_hash::FxHashSet;
use syntax::{Language, SyntaxNode, TextRange};

use crate::diagnostics::imports::ImportPlacement;
use crate::workspace_symbols::{MatchQuality, match_quality};

/// How many items a completion returns at most.
const LIMIT: usize = 200;

/// Names shorter than this don't get not-yet-imported suggestions: too
/// many to be useful.
const IMPORT_PREFIX_LENGTH: usize = 2;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    /// The declaration's signature.
    pub detail: Option<String>,
    /// What the item replaces: the name typed so far.
    pub replace: TextRange,
    /// The import to add with it (`a.b.Money`), and the edit adding it.
    pub import: Option<String>,
    pub import_edit: Option<crate::TextEdit>,
    /// Lower sorts first.
    pub relevance: u32,
    /// The declaration it names, whose documentation resolving it gives.
    pub declaration: Option<ItemLocation>,
    /// What accepting it writes when that isn't its label (`count = ` of
    /// a named argument).
    pub text: Option<String>,
    /// Shown right after the label: the rest of a declaration's signature
    /// (`(amount: Long): Unit`, `: String`).
    pub label_detail: Option<String>,
    /// Shown apart, less prominently: where it's declared (its class, its
    /// package) or the import accepting it adds.
    pub label_description: Option<String>,
}

impl CompletionItem {
    /// What accepting it writes: the name, in backticks where Kotlin needs
    /// them (`` `in` ``).
    pub fn insert_text(&self) -> std::borrow::Cow<'_, str> {
        match &self.text {
            Some(text) => std::borrow::Cow::Borrowed(text),
            None => syntax::names::escaped(&self.label),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompletionKind {
    Variable,
    Function,
    Method,
    Property,
    Field,
    Class,
    Interface,
    Enum,
    EnumMember,
    Object,
    TypeAlias,
    Package,
    /// `count = ` inside a call's parentheses.
    NamedArgument,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Completions {
    pub items: Vec<CompletionItem>,
    /// More would match: the client should ask again as the user types.
    pub incomplete: bool,
}

/// Where among the levels an item comes from, for ordering.
#[derive(Clone, Copy)]
enum Source {
    /// A parameter of the call the cursor is an argument of, by name.
    NamedArgument,
    Local,
    Member(MemberRank),
    /// An extension the qualifier can use: after its members.
    Extension,
    Scope,
    Import,
}

/// How likely a member is the one wanted, the likeliest first: what the
/// receiver's own class declares, then what it inherits, then what every
/// value has.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum MemberRank {
    OwnProperty,
    OwnFunction,
    InheritedProperty,
    InheritedFunction,
    /// `equals`, `hashCode`, `toString` — `Any`'s, or a class's overrides.
    Universal,
}

impl Source {
    /// Lower sorts first.
    fn relevance(self) -> u32 {
        match self {
            Source::NamedArgument | Source::Local => 0,
            Source::Member(rank) => 10 + rank as u32,
            Source::Extension => 20,
            Source::Scope => 30,
            Source::Import => 50,
        }
    }
}

pub(crate) fn completions(db: &RootDatabase, file: FileId, offset: usize) -> Completions {
    let Some(source) = db.source_file(file) else {
        return Completions::default();
    };
    if source.language(db) != Language::Kotlin {
        return Completions::default();
    }
    let text = source.text(db).clone();
    let offset = offset.min(text.len());
    let prefix_start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, char)| char.is_alphanumeric() || *char == '_')
        .last()
        .map_or(offset, |(index, _)| index);
    let prefix = &text[prefix_start..offset];
    let starts_with_digit = prefix
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_digit());
    if starts_with_digit {
        return Completions::default();
    }
    // A name to probe: the one being typed, or a placeholder where none is.
    let inserted = prefix.is_empty();
    let probed: Arc<str> = if inserted {
        Arc::from(format!(
            "{}{}{}",
            &text[..offset],
            hir::PLACEHOLDER,
            &text[offset..]
        ))
    } else {
        text.clone()
    };
    let parse = syntax::parse(probed.clone(), Language::Kotlin);
    let probe_offset = offset as u32;
    let replace = TextRange::new(prefix_start, offset);
    let mut builder = Builder {
        db,
        file: source,
        types: Types::new(db, source),
        prefix,
        replace,
        imports: ImportPlacement::of(&hir::parse(db, source)),
        items: Vec::new(),
        seen: FxHashSet::default(),
    };
    // The name being typed starts at `prefix_start` (the placeholder, when
    // inserted, at `offset` — the same place).
    let node = parse
        .root()
        .named_descendant_for_byte_range(prefix_start, prefix_start)
        .filter(|node| matches!(node.kind(), "simple_identifier" | "type_identifier"));
    let context = node
        .map(|node| context_of(node))
        .unwrap_or(Context::Expression);
    match context {
        Context::Import => builder.import_path(&parse, node),
        Context::Type => {
            builder.scope_classifiers();
            builder.importable(true);
        }
        Context::Expression => {
            if let Some(named) = node.and_then(|node| argument_names(&parse, node)) {
                builder.named_arguments(file, offset, &named);
            }
            let scope = hir::completion_scope(db, source, probed, probe_offset, inserted)
                .unwrap_or_default();
            builder.expression(&scope);
        }
    }
    builder.finish()
}

/// When `node` is a whole argument of a call not given by name (`pay(am|`),
/// the names of the call's arguments that are.
fn argument_names(parse: &syntax::Parse, node: SyntaxNode<'_>) -> Option<Vec<String>> {
    let argument = node
        .parent()
        .filter(|parent| parent.kind() == "value_argument")?;
    let alone = syntax::named_children(argument).count() == 1;
    if !alone || syntax::has_token(argument, "=") {
        return None;
    }
    let arguments = argument.parent()?;
    Some(
        syntax::named_children(arguments)
            .filter(|other| syntax::has_token(*other, "="))
            .filter_map(|other| syntax::named_children(other).next())
            .map(|name| parse.text_of(name).trim_matches('`').to_owned())
            .collect(),
    )
}

/// The documentation of the declaration the item `label` at `offset`
/// names.
pub(crate) fn documentation(
    db: &RootDatabase,
    file: FileId,
    offset: usize,
    label: &str,
) -> Option<String> {
    let item = completions(db, file, offset)
        .items
        .into_iter()
        .find(|item| item.label == label)?;
    crate::hover::documentation(db, item.declaration?)
}

enum Context {
    Expression,
    /// A type name (`: Money`, `is Money`, `<Money>`).
    Type,
    Import,
}

fn context_of(node: SyntaxNode<'_>) -> Context {
    let mut current = node.parent();
    while let Some(ancestor) = current {
        match ancestor.kind() {
            "import_header" | "package_header" => return Context::Import,
            "user_type" if node.kind() == "type_identifier" => return Context::Type,
            _ => {}
        }
        current = ancestor.parent();
    }
    Context::Expression
}

struct Builder<'db> {
    db: &'db RootDatabase,
    file: SourceFile,
    types: Types<'db>,
    prefix: &'db str,
    replace: TextRange,
    /// Where an import would go in the file.
    imports: ImportPlacement,
    items: Vec<(MatchQuality, CompletionItem)>,
    /// Labels already offered (a nearer level hides a farther one).
    seen: FxHashSet<String>,
}

impl<'db> Builder<'db> {
    fn matches(&self, name: &str) -> Option<MatchQuality> {
        if name == hir::PLACEHOLDER || name.starts_with('<') {
            return None;
        }
        if self.prefix.is_empty() {
            return Some(MatchQuality::Prefix);
        }
        match_quality(name, self.prefix).filter(|quality| *quality != MatchQuality::Subsequence)
    }

    /// Adds an item when the name typed matches it and no item has its
    /// label yet; whether it did.
    fn push(
        &mut self,
        label: String,
        kind: CompletionKind,
        detail: Option<String>,
        source: Source,
        import: Option<String>,
    ) -> bool {
        let Some(quality) = self.matches(&label) else {
            return false;
        };
        if !self.seen.insert(label.clone()) {
            return false;
        }
        self.items.push((
            quality,
            CompletionItem {
                label,
                kind,
                detail,
                replace: self.replace,
                import,
                import_edit: None,
                relevance: source.relevance(),
                declaration: None,
                text: None,
                label_detail: None,
                label_description: None,
            },
        ));
        true
    }

    /// `name = ` for each parameter of the call at `offset` not passed by
    /// name yet (`named`).
    fn named_arguments(&mut self, file: FileId, offset: usize, named: &[String]) {
        let Some(help) = crate::signature_help::signature_help(self.db, file, offset) else {
            return;
        };
        for signature in &help.signatures {
            for name in signature.parameter_names() {
                if named.contains(name) {
                    continue;
                }
                let label = format!("{name} =");
                let detail = Some(signature.label.clone());
                if self.push(
                    label,
                    CompletionKind::NamedArgument,
                    detail,
                    Source::NamedArgument,
                    None,
                ) && let Some((_, item)) = self.items.last_mut()
                {
                    item.text = Some(format!("{} = ", syntax::names::escaped(name)));
                }
            }
        }
    }

    fn push_declaration(&mut self, location: ItemLocation, source: Source, import: Option<String>) {
        let tree = hir::item_tree(self.db, location.container);
        let item = tree.item(location.item);
        if item.kind == ItemKind::Constructor {
            return;
        }
        let kind = kind_of(item.kind, item.parent.is_some());
        let pushed = self.push(
            item.name.to_string(),
            kind,
            Some(tree.signature(location.item)),
            source,
            import,
        );
        if pushed && let Some((_, pushed_item)) = self.items.last_mut() {
            pushed_item.declaration = Some(location);
        }
    }

    fn expression(&mut self, scope: &CompletionScope) {
        match &scope.qualifier {
            Some(Qualifier::Value(receiver)) => self.members(receiver, true),
            Some(Qualifier::Class(class)) => self.statics(*class),
            Some(Qualifier::Package(package)) => self.package_members(package),
            None => {
                for local in &scope.locals {
                    let detail =
                        format!("{}: {}", local.name, hir::display_type(self.db, &local.ty));
                    self.push(
                        local.name.to_string(),
                        CompletionKind::Variable,
                        Some(detail),
                        Source::Local,
                        None,
                    );
                }
                for receiver in &scope.receivers {
                    self.members(receiver, false);
                }
                self.scope_declarations();
                self.importable(false);
            }
        }
    }

    /// A value's members, Java getters as properties, and the extensions
    /// in scope it can use. A compiled data class's `componentN` is left
    /// out: it's for destructuring, not for calling.
    fn members(&mut self, receiver: &Ty, with_extensions: bool) {
        let own_classes: Vec<ItemLocation> = self
            .types
            .member_holders(receiver)
            .into_iter()
            .map(|holder| holder.class)
            .collect();
        for member in self.types.all_members(receiver) {
            let tree = hir::item_tree(self.db, member.location.container);
            let item = tree.item(member.location.item);
            if is_data_class_component(item) {
                continue;
            }
            let own = item.parent.is_some_and(|parent| {
                own_classes.contains(&ItemLocation {
                    container: member.location.container,
                    item: parent,
                })
            });
            if let Some(property) = self.types.java_property_name(member.location) {
                let detail = Some(tree.signature(member.location.item));
                let rank = member_rank(&property, true, own);
                self.push(
                    property,
                    CompletionKind::Property,
                    detail,
                    Source::Member(rank),
                    None,
                );
            }
            let is_property = matches!(item.kind, ItemKind::Property { .. });
            let rank = member_rank(item.name.as_str(), is_property, own);
            self.push_declaration(member.location, Source::Member(rank), None);
        }
        if with_extensions {
            self.extensions(receiver);
        }
    }

    /// Extensions declared at the levels the file sees that take `receiver`.
    fn extensions(&mut self, receiver: &Ty) {
        for package in self.scope_packages() {
            for name in hir::package_names(self.db, self.file, &package) {
                if self.matches(name.as_str()).is_none() {
                    continue;
                }
                for location in hir::visible_top_level(self.db, self.file, &package, name) {
                    if is_extension(self.db, location)
                        && hir::extends(self.db, self.file, location, receiver)
                    {
                        self.push_declaration(location, Source::Extension, None);
                    }
                }
            }
        }
    }

    /// `Class.`: nested classes, enum entries, Java statics, companion or
    /// object members.
    fn statics(&mut self, class: ItemLocation) {
        let tree = hir::item_tree(self.db, class.container);
        for child in tree.item(class.item).children.iter() {
            let item = tree.item(*child);
            let is_static = item.kind.is_classifier()
                || item.kind == ItemKind::EnumEntry
                || item.modifiers.contains(hir::Modifiers::STATIC);
            if is_static && item.kind != ItemKind::Class(hir::ClassKind::CompanionObject) {
                self.push_declaration(
                    ItemLocation {
                        container: class.container,
                        item: *child,
                    },
                    Source::Member(MemberRank::OwnProperty),
                    None,
                );
            }
        }
        if let Some(instance) = self.types.object_instance(class) {
            self.members(&Ty::Class(instance), true);
        }
    }

    /// `package.`: its declarations and subpackages.
    fn package_members(&mut self, package: &Path) {
        for name in hir::package_names(self.db, self.file, package) {
            for location in hir::visible_top_level(self.db, self.file, package, name) {
                if !is_extension(self.db, location) {
                    self.push_declaration(location, Source::Scope, None);
                }
            }
        }
    }

    /// The packages whose declarations are in scope without qualification:
    /// the file's own, its star imports, the default imports.
    fn scope_packages(&self) -> Vec<Path> {
        let tree = hir::file_item_tree(self.db, self.file);
        let mut packages = vec![tree.package.clone()];
        for import in tree.imports.iter() {
            if import.target == hir::ImportTarget::Everything {
                packages.push(import.path.clone());
            }
        }
        for default in hir::DEFAULT_IMPORT_PACKAGES
            .iter()
            .chain(hir::PLATFORM_DEFAULT_IMPORT_PACKAGES)
        {
            packages.push(Path::parse(default));
        }
        packages
    }

    /// The top-level declarations in scope: explicitly imported, then in
    /// the scope packages (not extensions: those need a receiver).
    fn scope_declarations(&mut self) {
        let tree = hir::file_item_tree(self.db, self.file);
        for import in tree.imports.iter() {
            let Some(visible) = import.visible_name() else {
                continue;
            };
            if self.matches(visible.as_str()).is_none() {
                continue;
            }
            for location in
                hir::resolve_import_path(self.db, self.file, &import.path).declarations()
            {
                if !is_extension(self.db, location) {
                    self.push_declaration(location, Source::Scope, None);
                }
            }
        }
        for package in self.scope_packages() {
            for name in hir::package_names(self.db, self.file, &package) {
                if self.matches(name.as_str()).is_none() {
                    continue;
                }
                for location in hir::visible_top_level(self.db, self.file, &package, name) {
                    if !is_extension(self.db, location) {
                        self.push_declaration(location, Source::Scope, None);
                    }
                }
            }
        }
    }

    /// The classes in scope (for a type position).
    fn scope_classifiers(&mut self) {
        let tree = hir::file_item_tree(self.db, self.file);
        for import in tree.imports.iter() {
            for location in
                hir::resolve_import_path(self.db, self.file, &import.path).declarations()
            {
                if hir::is_classifier(self.db, location) && import.visible_name().is_some() {
                    self.push_declaration(location, Source::Scope, None);
                }
            }
        }
        for package in self.scope_packages() {
            for name in hir::package_names(self.db, self.file, &package) {
                if self.matches(name.as_str()).is_none() {
                    continue;
                }
                for location in hir::visible_top_level(self.db, self.file, &package, name) {
                    if hir::is_classifier(self.db, location) {
                        self.push_declaration(location, Source::Scope, None);
                    }
                }
            }
        }
    }

    /// Declarations not imported yet whose names match, with their import.
    fn importable(&mut self, classifiers_only: bool) {
        if self.prefix.len() < IMPORT_PREFIX_LENGTH {
            return;
        }
        let in_scope: FxHashSet<Path> = self.scope_packages().into_iter().collect();
        let mut candidates: Vec<(MatchQuality, Name, Path)> =
            hir::importable_names(self.db, self.file)
                .into_iter()
                .filter(|(name, package)| {
                    !in_scope.contains(package) && !self.seen.contains(name.as_str())
                })
                .filter_map(|(name, package)| Some((self.matches(name.as_str())?, name, package)))
                .collect();
        candidates.sort_by(|left, right| {
            (left.0, left.1.as_str().len(), left.1, &left.2).cmp(&(
                right.0,
                right.1.as_str().len(),
                right.1,
                &right.2,
            ))
        });
        candidates.dedup_by(|left, right| left.1 == right.1 && left.2 == right.2);
        for (quality, name, package) in candidates.into_iter().take(LIMIT) {
            let qualified = package.join(name).to_string();
            let declarations = hir::visible_top_level(self.db, self.file, &package, name);
            let fitting = declarations.into_iter().find(|location| {
                if classifiers_only {
                    hir::is_classifier(self.db, *location)
                } else {
                    !is_extension(self.db, *location)
                }
            });
            let Some(location) = fitting else {
                continue;
            };
            let tree = hir::item_tree(self.db, location.container);
            let item = tree.item(location.item);
            self.items.push((
                quality,
                CompletionItem {
                    label: name.to_string(),
                    kind: kind_of(item.kind, item.parent.is_some()),
                    detail: Some(format!(
                        "{} (import {qualified})",
                        tree.signature(location.item)
                    )),
                    replace: self.replace,
                    import_edit: Some(self.imports.edit(&qualified)),
                    import: Some(qualified),
                    relevance: Source::Import.relevance(),
                    declaration: Some(location),
                    text: None,
                    label_detail: None,
                    label_description: None,
                },
            ));
        }
    }

    /// `import a.b.|`: the package's classes and subpackages.
    fn import_path(&mut self, parse: &syntax::Parse, node: Option<SyntaxNode<'_>>) {
        let Some(node) = node else {
            return;
        };
        let Some(path_node) = node.parent().filter(|parent| parent.kind() == "identifier") else {
            return;
        };
        let segments: Vec<Name> = syntax::named_children(path_node)
            .take_while(|segment| segment.id() != node.id())
            .map(|segment| Name::new(parse.text_of(segment)))
            .collect();
        let package = Path::new(segments);
        for name in hir::package_names(self.db, self.file, &package) {
            for location in hir::visible_top_level(self.db, self.file, &package, name) {
                self.push_declaration(location, Source::Scope, None);
            }
        }
    }

    fn finish(self) -> Completions {
        let mut items = self.items;
        for (_, item) in &mut items {
            let (detail, description) = label_details(self.db, item);
            item.label_detail = detail;
            item.label_description = description;
        }
        // Stable: within a level, items keep the order they were found in —
        // a class's own members before inherited ones, `Any`'s last.
        items.sort_by_key(|(quality, item)| (item.relevance, *quality));
        let incomplete = items.len() > LIMIT || self.prefix.len() < IMPORT_PREFIX_LENGTH;
        Completions {
            items: items
                .into_iter()
                .take(LIMIT)
                .map(|(_, item)| item)
                .collect(),
            incomplete,
        }
    }
}

/// Where a member ranks: a name every value has (`toString`) last, else
/// own before inherited, properties before functions.
fn member_rank(name: &str, is_property: bool, own: bool) -> MemberRank {
    let universal = !is_property && matches!(name, "equals" | "hashCode" | "toString");
    match (universal, own, is_property) {
        (true, _, _) => MemberRank::Universal,
        (false, true, true) => MemberRank::OwnProperty,
        (false, true, false) => MemberRank::OwnFunction,
        (false, false, true) => MemberRank::InheritedProperty,
        (false, false, false) => MemberRank::InheritedFunction,
    }
}

/// A compiled data class's `componentN()`, which the compiler generates
/// for destructuring.
fn is_data_class_component(item: &hir::Item) -> bool {
    let generated = item.modifiers.contains(hir::Modifiers::SYNTHESIZED);
    let numbered = item
        .name
        .as_str()
        .strip_prefix("component")
        .is_some_and(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        });
    generated && numbered
}

fn is_extension(db: &RootDatabase, location: ItemLocation) -> bool {
    let tree = hir::item_tree(db, location.container);
    matches!(
        &tree.item(location.item).signature,
        Signature::Function {
            receiver: Some(_),
            ..
        } | Signature::Property {
            receiver: Some(_),
            ..
        }
    )
}

fn kind_of(kind: ItemKind, is_member: bool) -> CompletionKind {
    use hir::ClassKind;
    match kind {
        ItemKind::Class(ClassKind::Interface | ClassKind::Annotation) => CompletionKind::Interface,
        ItemKind::Class(ClassKind::Enum) => CompletionKind::Enum,
        ItemKind::Class(ClassKind::Object | ClassKind::CompanionObject) => CompletionKind::Object,
        ItemKind::Class(ClassKind::Class) => CompletionKind::Class,
        ItemKind::Function if is_member => CompletionKind::Method,
        ItemKind::Function | ItemKind::Constructor => CompletionKind::Function,
        ItemKind::Property { .. } if is_member => CompletionKind::Field,
        ItemKind::Property { .. } => CompletionKind::Property,
        ItemKind::TypeAlias => CompletionKind::TypeAlias,
        ItemKind::EnumEntry => CompletionKind::EnumMember,
    }
}

/// What follows an item's label (the rest of its signature) and what's
/// shown apart (where it's declared, or the import it adds).
fn label_details(db: &RootDatabase, item: &CompletionItem) -> (Option<String>, Option<String>) {
    let import = item
        .import
        .as_ref()
        .map(|import| format!("import {import}"));
    let Some(location) = item.declaration else {
        // A local: `name: Type` as its detail.
        let after_name = item
            .detail
            .as_deref()
            .and_then(|detail| detail.strip_prefix(item.label.as_str()))
            .filter(|rest| rest.starts_with(':'))
            .map(str::to_owned);
        return (after_name, import);
    };
    let tree = hir::item_tree(db, location.container);
    let declared = tree.item(location.item);
    let signature = tree.signature(location.item);
    let name = declared.name.as_str();
    let after_name = [format!("{name}("), format!("{name}:"), format!("{name}<")]
        .iter()
        .find_map(|start| signature.find(start.as_str()))
        .map(|start| signature[start + name.len()..].to_owned());
    let container = match declared.parent {
        Some(parent) => Some(tree.item(parent).name.to_string()),
        None if tree.package.is_root() => None,
        None => Some(tree.package.to_string()),
    };
    (after_name, import.or(container))
}
