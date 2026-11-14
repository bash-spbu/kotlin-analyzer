//! `@file:Suppress("…")`: the names a file turns its findings off with.

use rustc_hash::FxHashSet;
use syntax::{Parse, named_children};

/// The strings of the file's `@file:Suppress(…)` annotations.
pub(super) fn file_suppressions(parse: &Parse) -> FxHashSet<String> {
    let mut names = FxHashSet::default();
    let annotations = named_children(parse.root()).filter(|node| node.kind() == "file_annotation");
    for annotation in annotations.flat_map(named_children) {
        let is_suppress = named_children(annotation)
            .find(|child| child.kind() == "user_type")
            .is_some_and(|user_type| {
                let written = parse.text_of(user_type);
                written == "Suppress" || written == "kotlin.Suppress"
            });
        if !is_suppress {
            continue;
        }
        let strings =
            syntax::descendants(annotation).filter(|node| node.kind() == "string_content");
        names.extend(strings.map(|string| parse.text_of(string).to_owned()));
    }
    names
}
