//! Workspace-wide counts for `analysis-stats`: how many declarations there
//! are and how many type names written in their signatures resolve; how
//! many names in bodies inference resolves, and how long it takes.

use std::time::{Duration, Instant};

use hir::{
    ItemTree, Name, Resolution, RootDatabase, Signature, SourceDatabase, SourceFile, TypeRef,
};
use rustc_hash::FxHashMap;

#[derive(Debug, Default)]
pub struct DeclarationStatistics {
    pub declarations: usize,
    pub type_references: usize,
    pub resolved: usize,
    pub type_parameters: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    /// Unresolved first segments by how often they occur, most first.
    pub top_unresolved: Vec<(String, usize)>,
    /// Ambiguous first segments by how often they occur, most first.
    pub top_ambiguous: Vec<(String, usize)>,
}

pub(crate) fn declaration_statistics(db: &RootDatabase) -> DeclarationStatistics {
    let mut statistics = DeclarationStatistics::default();
    let mut unresolved_names: FxHashMap<Name, usize> = FxHashMap::default();
    let mut ambiguous_names: FxHashMap<Name, usize> = FxHashMap::default();
    for file in db.all_files() {
        let tree = hir::file_item_tree(db, file);
        statistics.declarations += tree.items.len();
        for type_ref in written_types(&tree) {
            count(
                db,
                file,
                type_ref,
                &mut statistics,
                &mut unresolved_names,
                &mut ambiguous_names,
            );
        }
    }
    statistics.top_unresolved = ranked(unresolved_names);
    statistics.top_ambiguous = ranked(ambiguous_names);
    statistics
}

fn ranked(counts: FxHashMap<Name, usize>) -> Vec<(String, usize)> {
    let mut top: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(name, count)| (name.to_string(), count))
        .collect();
    top.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    top
}

/// Every type reference in the tree's signatures, type arguments included.
fn written_types(tree: &ItemTree) -> Vec<&TypeRef> {
    let mut found = Vec::new();
    for item in tree.items.iter() {
        for parameter in item.type_parameters.iter() {
            found.extend(parameter.bounds.iter());
        }
        match &item.signature {
            Signature::Class {
                supertypes,
                primary_constructor,
            } => {
                found.extend(supertypes.iter());
                for parameter in primary_constructor
                    .iter()
                    .flat_map(|parameters| parameters.iter())
                {
                    found.extend(parameter.type_ref.iter());
                }
            }
            Signature::Function {
                receiver,
                parameters,
                return_type,
            } => {
                found.extend(receiver.iter());
                for parameter in parameters.iter() {
                    found.extend(parameter.type_ref.iter());
                }
                found.extend(return_type.iter());
            }
            Signature::Property { receiver, type_ref } => {
                found.extend(receiver.iter());
                found.extend(type_ref.iter());
            }
            Signature::TypeAlias { target } => found.push(target),
            Signature::None => {}
        }
    }
    let mut nested = Vec::new();
    for type_ref in &found {
        collect_nested(type_ref, &mut nested);
    }
    nested
}

fn collect_nested<'tree>(type_ref: &'tree TypeRef, output: &mut Vec<&'tree TypeRef>) {
    match type_ref {
        TypeRef::Named { segments, .. } => {
            output.push(type_ref);
            for segment in segments.iter() {
                for argument in segment.arguments.iter() {
                    if let hir::TypeArgument::Type { type_ref, .. } = argument {
                        collect_nested(type_ref, output);
                    }
                }
            }
        }
        TypeRef::Function {
            receiver,
            parameters,
            result,
            ..
        } => {
            if let Some(receiver) = receiver {
                collect_nested(receiver, output);
            }
            for parameter in parameters.iter() {
                collect_nested(parameter, output);
            }
            collect_nested(result, output);
        }
        TypeRef::Array { element } => collect_nested(element, output),
        TypeRef::Qualified { .. }
        | TypeRef::TypeParameter { .. }
        | TypeRef::Primitive { .. }
        | TypeRef::Unreadable => {}
    }
}

fn count(
    db: &RootDatabase,
    file: SourceFile,
    type_ref: &TypeRef,
    statistics: &mut DeclarationStatistics,
    unresolved_names: &mut FxHashMap<Name, usize>,
    ambiguous_names: &mut FxHashMap<Name, usize>,
) {
    let TypeRef::Named { range, .. } = type_ref else {
        return;
    };
    let Some(path) = type_ref.path() else {
        return;
    };
    statistics.type_references += 1;
    match hir::resolve_type_path(db, file, &path, range.start as usize) {
        Resolution::Declaration(_) => statistics.resolved += 1,
        Resolution::TypeParameter { .. } => statistics.type_parameters += 1,
        Resolution::Ambiguous(_) => {
            statistics.ambiguous += 1;
            if let Some(first) = path.segments().first() {
                *ambiguous_names.entry(*first).or_default() += 1;
            }
        }
        Resolution::Package(_) | Resolution::Unresolved => {
            statistics.unresolved += 1;
            if let Some(first) = path.segments().first() {
                *unresolved_names.entry(*first).or_default() += 1;
            }
        }
    }
}

/// Computes the item trees of `files`, so later queries find them cached.
pub(crate) fn prime_item_trees(db: &RootDatabase, files: &[hir::FileId]) {
    for file in files {
        if let Some(source) = db.source_file(*file) {
            hir::file_item_tree(db, source);
        }
    }
}

/// What inference found in bodies.
#[derive(Debug, Default)]
pub struct BodyStatistics {
    pub files: usize,
    /// Names resolved to one declaration or local.
    pub resolved: usize,
    /// Of those, the ones whose value's type is known.
    pub typed: usize,
    /// Names with several candidates left (ambiguous, or none fitting).
    pub candidates: usize,
    pub unresolved: usize,
    pub unresolved_names: FxHashMap<String, usize>,
    pub candidate_names: FxHashMap<String, usize>,
    /// Where each unsettled name first occurs.
    pub examples: FxHashMap<String, (hir::FileId, u32)>,
    /// How long each file's inference took.
    pub durations: Vec<(hir::FileId, Duration)>,
}

impl BodyStatistics {
    pub fn merge(&mut self, other: BodyStatistics) {
        self.files += other.files;
        self.resolved += other.resolved;
        self.typed += other.typed;
        self.candidates += other.candidates;
        self.unresolved += other.unresolved;
        for (name, count) in other.unresolved_names {
            *self.unresolved_names.entry(name).or_default() += count;
        }
        for (name, count) in other.candidate_names {
            *self.candidate_names.entry(name).or_default() += count;
        }
        for (name, example) in other.examples {
            self.examples.entry(name).or_insert(example);
        }
        self.durations.extend(other.durations);
    }

    /// Names by how often they occur, most first.
    pub fn ranked(counts: &FxHashMap<String, usize>) -> Vec<(String, usize)> {
        let mut top: Vec<(String, usize)> = counts
            .iter()
            .map(|(name, count)| (name.clone(), *count))
            .collect();
        top.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
        top
    }
}

pub(crate) fn body_statistics(db: &RootDatabase, files: &[hir::FileId]) -> BodyStatistics {
    let mut statistics = BodyStatistics::default();
    for file_id in files {
        let Some(file) = db.source_file(*file_id) else {
            continue;
        };
        if file.language(db) != hir::Language::Kotlin {
            continue;
        }
        let started = Instant::now();
        let inference = hir::infer_file(db, file);
        statistics.durations.push((*file_id, started.elapsed()));
        statistics.files += 1;
        let text = file.text(db);
        let name_at = |start: u32| -> String {
            text[start as usize..]
                .chars()
                .take_while(|char| char.is_alphanumeric() || *char == '_')
                .collect()
        };
        for (start, resolved) in inference.names() {
            match resolved.reference {
                hir::Reference::Local(_) | hir::Reference::Declaration(_) => {
                    statistics.resolved += 1;
                    if !resolved.ty.is_unknown() {
                        statistics.typed += 1;
                    }
                }
                hir::Reference::Candidates(_) => {
                    statistics.candidates += 1;
                    let name = name_at(start);
                    statistics
                        .examples
                        .entry(name.clone())
                        .or_insert((*file_id, start));
                    *statistics.candidate_names.entry(name).or_default() += 1;
                }
            }
        }
        for (start, name) in inference.unresolved() {
            statistics
                .examples
                .entry(name.to_string())
                .or_insert((*file_id, *start));
            statistics.unresolved += 1;
            *statistics
                .unresolved_names
                .entry(name.to_string())
                .or_default() += 1;
        }
    }
    statistics
}

/// A name in a body inference couldn't settle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnsettledName {
    pub offset: u32,
    pub name: String,
    /// `None`: nothing has the name; else how many candidates are left.
    pub candidates: Option<usize>,
}

pub(crate) fn unsettled_names(db: &RootDatabase, file: hir::FileId) -> Vec<UnsettledName> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let inference = hir::infer_file(db, source);
    let text = source.text(db);
    let name_at = |start: u32| -> String {
        text[start as usize..]
            .chars()
            .take_while(|char| char.is_alphanumeric() || *char == '_')
            .collect()
    };
    let mut unsettled: Vec<UnsettledName> = inference
        .names()
        .filter_map(|(start, resolved)| match &resolved.reference {
            hir::Reference::Candidates(candidates) => Some(UnsettledName {
                offset: start,
                name: name_at(start),
                candidates: Some(candidates.len()),
            }),
            _ => None,
        })
        .chain(
            inference
                .unresolved()
                .iter()
                .map(|(start, name)| UnsettledName {
                    offset: *start,
                    name: name.to_string(),
                    candidates: None,
                }),
        )
        .collect();
    unsettled.sort_by_key(|name| name.offset);
    unsettled
}
