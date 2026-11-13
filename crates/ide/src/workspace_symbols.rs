//! `workspace/symbol`: declarations across the workspace whose names match a
//! query — exactly, by prefix, by camel-case humps, or as a subsequence.

use hir::{ItemKind, ItemLocation, RootDatabase, SourceDatabase};

use crate::NavigationTarget;

/// How well a name matches; lower is better.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum MatchQuality {
    Exact,
    Prefix,
    CamelHumps,
    Substring,
    Subsequence,
}

pub(crate) fn workspace_symbols(
    db: &RootDatabase,
    query: &str,
    limit: usize,
) -> Vec<NavigationTarget> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<(MatchQuality, usize, ItemLocation)> = Vec::new();
    for source_set in db.workspace().source_sets(db).iter() {
        for file in source_set.files(db).iter() {
            let tree = hir::file_item_tree(db, *file);
            for (index, item) in tree.indexed_items() {
                if matches!(item.kind, ItemKind::Constructor) {
                    continue;
                }
                let name = item.name.as_str();
                if let Some(quality) = match_quality(name, query) {
                    matches.push((
                        quality,
                        name.len(),
                        ItemLocation {
                            file: *file,
                            item: index,
                        },
                    ));
                }
            }
        }
    }
    matches.sort_by_key(|(quality, length, _)| (*quality, *length));
    matches
        .into_iter()
        .take(limit)
        .map(|(_, _, location)| NavigationTarget::of(db, location))
        .collect()
}

fn match_quality(name: &str, query: &str) -> Option<MatchQuality> {
    if name == query {
        return Some(MatchQuality::Exact);
    }
    let lower_name = name.to_lowercase();
    let lower_query = query.to_lowercase();
    if lower_name.starts_with(&lower_query) {
        return Some(MatchQuality::Prefix);
    }
    if matches_camel_humps(name, query) {
        return Some(MatchQuality::CamelHumps);
    }
    if lower_name.contains(&lower_query) {
        return Some(MatchQuality::Substring);
    }
    let mut remaining = lower_name.chars();
    let is_subsequence = lower_query
        .chars()
        .all(|wanted| remaining.by_ref().any(|character| character == wanted));
    is_subsequence.then_some(MatchQuality::Subsequence)
}

/// `DRDRS` matches `DailyRateDeviationReportingService`: the query's
/// letters are the name's capitals in order.
fn matches_camel_humps(name: &str, query: &str) -> bool {
    let humps: String = name
        .chars()
        .filter(|character| character.is_uppercase())
        .collect();
    !humps.is_empty() && humps.starts_with(&query.to_uppercase()) && query.len() > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_quality_orders_exact_prefix_humps_substring_subsequence() {
        let name = "DailyRateDeviationReportingService";
        assert_eq!(match_quality(name, name), Some(MatchQuality::Exact));
        assert_eq!(match_quality(name, "dailyrate"), Some(MatchQuality::Prefix));
        assert_eq!(match_quality(name, "DRDR"), Some(MatchQuality::CamelHumps));
        assert_eq!(
            match_quality(name, "Deviation"),
            Some(MatchQuality::Substring)
        );
        assert_eq!(
            match_quality(name, "drsvc"),
            Some(MatchQuality::Subsequence)
        );
        assert_eq!(match_quality(name, "xyz"), None);
    }
}
