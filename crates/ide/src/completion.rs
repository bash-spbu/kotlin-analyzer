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
    Local = 0,
    Member = 1,
    /// An extension the qualifier can use: after its own members.
    Extension = 2,
    Scope = 3,
    Import = 5,
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
            let scope = hir::completion_scope(db, source, probed, probe_offset, inserted)
                .unwrap_or_default();
            builder.expression(&scope);
        }
    }
    builder.finish()
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

    fn push(
        &mut self,
        label: String,
        kind: CompletionKind,
        detail: Option<String>,
        source: Source,
        import: Option<String>,
    ) {
        let Some(quality) = self.matches(&label) else {
            return;
        };
        if !self.seen.insert(label.clone()) {
            return;
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
                relevance: source as u32,
            },
        ));
    }

    fn push_declaration(&mut self, location: ItemLocation, source: Source, import: Option<String>) {
        let tree = hir::item_tree(self.db, location.container);
        let item = tree.item(location.item);
        if item.kind == ItemKind::Constructor {
            return;
        }
        let kind = kind_of(item.kind, item.parent.is_some());
        self.push(
            item.name.to_string(),
            kind,
            Some(tree.signature(location.item)),
            source,
            import,
        );
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
    /// in scope it can use.
    fn members(&mut self, receiver: &Ty, with_extensions: bool) {
        for member in self.types.all_members(receiver) {
            if let Some(property) = self.types.java_property_name(member.location) {
                let tree = hir::item_tree(self.db, member.location.container);
                let detail = Some(tree.signature(member.location.item));
                self.push(
                    property,
                    CompletionKind::Property,
                    detail,
                    Source::Member,
                    None,
                );
            }
            self.push_declaration(member.location, Source::Member, None);
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
                    Source::Member,
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
                if is_classifier(self.db, location) && import.visible_name().is_some() {
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
                    if is_classifier(self.db, location) {
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
                    is_classifier(self.db, *location)
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
                    relevance: Source::Import as u32,
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

fn is_classifier(db: &RootDatabase, location: ItemLocation) -> bool {
    hir::item_tree(db, location.container)
        .item(location.item)
        .kind
        .is_classifier()
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
