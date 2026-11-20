//! `workspace/symbol`: declarations across the workspace whose names match a
//! query — exactly, by prefix, by camel-case humps, or as a subsequence —
//! then, for a query of a few characters, library classes (the JDK's
//! included) whose names match it closely.

use hir::{ItemKind, ItemLocation, LibraryName, RootDatabase, SourceDatabase};

use crate::NavigationTarget;

/// How well a name matches; lower is better.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum MatchQuality {
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
                    matches.push((quality, name.len(), ItemLocation::in_file(*file, index)));
                }
            }
        }
    }
    matches.sort_by_key(|(quality, length, _)| (*quality, *length));
    let mut found: Vec<NavigationTarget> = matches
        .into_iter()
        .take(limit)
        .filter_map(|(_, _, location)| NavigationTarget::of(db, location))
        .collect();
    let asks_libraries = query.chars().count() >= LIBRARY_QUERY_MINIMUM;
    if asks_libraries {
        found.extend(library_classes(db, query));
    }
    found
}

/// How long a query is before library classes are searched too: shorter
/// ones match too many.
const LIBRARY_QUERY_MINIMUM: usize = 3;

/// How many library classes a query gets at most: each is read from its
/// library (its declarations, then its source or its class file rendered)
/// before the answer goes out.
const LIBRARY_LIMIT: usize = 20;

/// The library classes whose names match `query` exactly, by prefix or by
/// camel-case humps — not looser: the libraries' names are many — best
/// first; among equal names, the shortest package first (a library's own
/// before a copy another shades: `com.fasterxml.jackson.databind` before
/// `org.testcontainers.shaded.com.fasterxml.jackson.databind`). One class
/// per qualified name, however many libraries hold it.
fn library_classes(db: &RootDatabase, query: &str) -> Vec<NavigationTarget> {
    let close = |name: &str| {
        match_quality(name, query).is_some_and(|quality| quality <= MatchQuality::CamelHumps)
    };
    let mut names: Vec<(MatchQuality, usize, usize, String, LibraryName)> =
        hir::library_names_matching(db, close)
            .into_iter()
            .filter_map(|name| {
                let quality = match_quality(name.name.as_str(), query)?;
                let package = name.package.to_string();
                Some((
                    quality,
                    name.name.as_str().len(),
                    package.len(),
                    package,
                    name,
                ))
            })
            .collect();
    names.sort_by(|first, second| {
        let (quality, length, package_length, package, name) = first;
        let (other_quality, other_length, other_package_length, other_package, other_name) = second;
        (quality, length, package_length, package, name.name.as_str()).cmp(&(
            other_quality,
            other_length,
            other_package_length,
            other_package,
            other_name.name.as_str(),
        ))
    });
    let mut found = Vec::new();
    for (_, _, _, _, name) in names {
        if found.len() >= LIBRARY_LIMIT {
            break;
        }
        // A function's or property's name is no class: nothing to show.
        if let Some(class) = hir::library_classes_in(db, &name).into_iter().next()
            && let Some(target) = NavigationTarget::of(db, class)
        {
            found.push(target);
        }
    }
    found
}

pub(crate) fn match_quality(name: &str, query: &str) -> Option<MatchQuality> {
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

/// `WRFRS` matches `WeeklyRateForecastReportingService`: the query's
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
        let name = "WeeklyRateForecastReportingService";
        assert_eq!(match_quality(name, name), Some(MatchQuality::Exact));
        assert_eq!(
            match_quality(name, "weeklyrate"),
            Some(MatchQuality::Prefix)
        );
        assert_eq!(match_quality(name, "WRFR"), Some(MatchQuality::CamelHumps));
        assert_eq!(
            match_quality(name, "Forecast"),
            Some(MatchQuality::Substring)
        );
        assert_eq!(
            match_quality(name, "wrsvc"),
            Some(MatchQuality::Subsequence)
        );
        assert_eq!(match_quality(name, "xyz"), None);
    }
}
