//! `textDocument/prepareTypeHierarchy`, `typeHierarchy/supertypes`,
//! `typeHierarchy/subtypes`, `textDocument/prepareCallHierarchy`,
//! `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`. An item's
//! `data` holds what finds its declaration again (see `ide::HierarchyKey`),
//! with the file to look from as its `textDocument`.

use ide::{Cancellable, HierarchyCall, HierarchyItem, HierarchyKey};
use serde_json::{Value, json};
use vfs::{FileId, LineIndex};

use super::convert;
use super::handlers::{Snapshot, symbol_kind, target_ranges};

pub(super) const PREPARE_TYPE_HIERARCHY: &str = "textDocument/prepareTypeHierarchy";
pub(super) const SUPERTYPES: &str = "typeHierarchy/supertypes";
pub(super) const SUBTYPES: &str = "typeHierarchy/subtypes";
pub(super) const PREPARE_CALL_HIERARCHY: &str = "textDocument/prepareCallHierarchy";
pub(super) const INCOMING_CALLS: &str = "callHierarchy/incomingCalls";
pub(super) const OUTGOING_CALLS: &str = "callHierarchy/outgoingCalls";

pub(super) const METHODS: [&str; 6] = [
    PREPARE_TYPE_HIERARCHY,
    SUPERTYPES,
    SUBTYPES,
    PREPARE_CALL_HIERARCHY,
    INCOMING_CALLS,
    OUTGOING_CALLS,
];

pub(super) fn handle(snapshot: &Snapshot, method: &str, params: &Value) -> Cancellable<Value> {
    let Some(file) = snapshot.request_file.filter(|_| snapshot.loaded) else {
        return Ok(Value::Null);
    };
    match method {
        PREPARE_TYPE_HIERARCHY | PREPARE_CALL_HIERARCHY => {
            let Some(offset) = position_offset(snapshot, file, params)? else {
                return Ok(Value::Null);
            };
            let items = if method == PREPARE_TYPE_HIERARCHY {
                snapshot.analysis.prepare_type_hierarchy(file, offset)?
            } else {
                snapshot.analysis.prepare_call_hierarchy(file, offset)?
            };
            items_json(snapshot, file, &items)
        }
        SUPERTYPES | SUBTYPES => {
            let Some(key) = key_of(snapshot, params) else {
                return Ok(Value::Null);
            };
            let items = if method == SUPERTYPES {
                snapshot.analysis.supertypes(file, &key)?
            } else {
                snapshot.analysis.subtypes(file, &key)?
            };
            items_json(snapshot, file, &items)
        }
        INCOMING_CALLS | OUTGOING_CALLS => {
            let Some(key) = key_of(snapshot, params) else {
                return Ok(Value::Null);
            };
            let incoming = method == INCOMING_CALLS;
            let calls = if incoming {
                snapshot.analysis.incoming_calls(&key)?
            } else {
                snapshot.analysis.outgoing_calls(&key)?
            };
            calls_json(snapshot, file, &calls, incoming)
        }
        _ => Ok(Value::Null),
    }
}

/// The request's position as an offset in its file.
fn position_offset(
    snapshot: &Snapshot,
    file: FileId,
    params: &Value,
) -> Cancellable<Option<usize>> {
    let Ok(position) = serde_json::from_value::<lsp_types::Position>(params["position"].clone())
    else {
        return Ok(None);
    };
    let Some(text) = snapshot.analysis.file_text(file)? else {
        return Ok(None);
    };
    Ok(Some(convert::offset(&text, position, snapshot.encoding)))
}

fn items_json(snapshot: &Snapshot, from: FileId, items: &[HierarchyItem]) -> Cancellable<Value> {
    let mut json_items = Vec::with_capacity(items.len());
    for item in items {
        json_items.extend(item_json(snapshot, from, item)?);
    }
    Ok(Value::Array(json_items))
}

fn calls_json(
    snapshot: &Snapshot,
    from: FileId,
    calls: &[HierarchyCall],
    incoming: bool,
) -> Cancellable<Value> {
    let mut json_calls = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(item) = item_json(snapshot, from, &call.item)? else {
            continue;
        };
        let mut ranges = Vec::with_capacity(call.ranges.len());
        for range in &call.ranges {
            let Some(text) = snapshot.analysis.file_text(range.file)? else {
                continue;
            };
            let index = LineIndex::with_unit(&text, snapshot.encoding);
            ranges.push(convert::range(&index, &text, range.range));
        }
        let field = if incoming { "from" } else { "to" };
        json_calls.push(json!({ field: item, "fromRanges": ranges }));
    }
    Ok(Value::Array(json_calls))
}

/// An item as LSP has it, its key in `data` (looked up from `from`).
fn item_json(
    snapshot: &Snapshot,
    from: FileId,
    item: &HierarchyItem,
) -> Cancellable<Option<Value>> {
    let Some(ranges) = target_ranges(snapshot, &item.target)? else {
        return Ok(None);
    };
    let uri_of = |file: FileId| {
        snapshot
            .paths
            .get(file.index())
            .and_then(|path| convert::uri_of(path))
            .map(|uri| uri.as_str().to_owned())
    };
    let data = match &item.key {
        Some(HierarchyKey::Class { package, names }) => uri_of(from).map(
            |from| json!({ "textDocument": { "uri": from }, "package": package, "names": names }),
        ),
        Some(HierarchyKey::Declaration { file, offset }) => {
            uri_of(*file).map(|uri| json!({ "textDocument": { "uri": uri }, "offset": offset }))
        }
        None => None,
    };
    Ok(Some(json!({
        "name": item.target.name,
        "kind": symbol_kind(item.target.kind),
        "detail": item.target.container,
        "uri": ranges.uri,
        "range": ranges.full,
        "selectionRange": ranges.focus,
        "data": data,
    })))
}

/// The key in a request's `item.data`; a declaration's in the request's
/// file.
fn key_of(snapshot: &Snapshot, params: &Value) -> Option<HierarchyKey> {
    let data = &params["item"]["data"];
    if let Some(offset) = data["offset"].as_u64() {
        return Some(HierarchyKey::Declaration {
            file: snapshot.request_file?,
            offset: offset as u32,
        });
    }
    let package = data["package"].as_str()?.to_owned();
    let names = data["names"]
        .as_array()?
        .iter()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect();
    Some(HierarchyKey::Class { package, names })
}
