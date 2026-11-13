//! Workspace-wide counts for `analysis-stats`: how many declarations there
//! are and how many type names written in their signatures resolve.

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
}

pub(crate) fn declaration_statistics(db: &RootDatabase) -> DeclarationStatistics {
    let mut statistics = DeclarationStatistics::default();
    let mut unresolved_names: FxHashMap<Name, usize> = FxHashMap::default();
    for file in db.all_files() {
        let tree = hir::file_item_tree(db, file);
        statistics.declarations += tree.items.len();
        for type_ref in written_types(&tree) {
            count(db, file, type_ref, &mut statistics, &mut unresolved_names);
        }
    }
    let mut top: Vec<(String, usize)> = unresolved_names
        .into_iter()
        .map(|(name, count)| (name.to_string(), count))
        .collect();
    top.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    statistics.top_unresolved = top;
    statistics
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
        TypeRef::Primitive { .. } | TypeRef::Unreadable => {}
    }
}

fn count(
    db: &RootDatabase,
    file: SourceFile,
    type_ref: &TypeRef,
    statistics: &mut DeclarationStatistics,
    unresolved_names: &mut FxHashMap<Name, usize>,
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
        Resolution::Ambiguous(_) => statistics.ambiguous += 1,
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
