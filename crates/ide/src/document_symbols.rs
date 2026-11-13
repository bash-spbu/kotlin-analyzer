//! `textDocument/documentSymbol`: the file's declarations as a tree.

use hir::{FileId, ItemId, ItemTree, RootDatabase, Signature, SourceDatabase};
use syntax::TextRange;

use crate::SymbolKind;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The signature, briefly: a type or a parameter list.
    pub detail: Option<String>,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub deprecated: bool,
    pub children: Vec<DocumentSymbol>,
}

pub(crate) fn document_symbols(db: &RootDatabase, file: FileId) -> Vec<DocumentSymbol> {
    let Some(source) = db.source_file(file) else {
        return Vec::new();
    };
    let tree = hir::file_item_tree(db, source);
    let source_map = hir::file_item_source_map(db, source);
    tree.top_level
        .iter()
        .map(|index| symbol(&tree, &source_map, *index))
        .collect()
}

fn symbol(tree: &ItemTree, source_map: &hir::ItemSourceMap, index: ItemId) -> DocumentSymbol {
    let item = tree.item(index);
    let ranges = source_map.ranges(index);
    DocumentSymbol {
        name: item.name.to_string(),
        kind: SymbolKind::of(item.kind, item.parent.is_some()),
        detail: detail(&item.signature),
        range: ranges.range,
        selection_range: ranges.name_range,
        deprecated: item.modifiers.contains(hir::Modifiers::DEPRECATED),
        children: item
            .children
            .iter()
            .map(|child| symbol(tree, source_map, *child))
            .collect(),
    }
}

fn detail(signature: &Signature) -> Option<String> {
    match signature {
        Signature::Function {
            parameters,
            return_type,
            ..
        } => {
            let parameters: Vec<String> = parameters
                .iter()
                .map(|parameter| match &parameter.type_ref {
                    Some(type_ref) => {
                        format!("{}: {}", parameter.name, hir::type_ref_text(type_ref))
                    }
                    None => parameter.name.to_string(),
                })
                .collect();
            let result = return_type
                .as_ref()
                .map(|type_ref| format!(": {}", hir::type_ref_text(type_ref)))
                .unwrap_or_default();
            Some(format!("({}){result}", parameters.join(", ")))
        }
        Signature::Property {
            type_ref: Some(type_ref),
            ..
        } => Some(hir::type_ref_text(type_ref)),
        _ => None,
    }
}
