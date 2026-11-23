//! What the database keeps, by query and by kind of stored value, as Salsa
//! counts it: cheap to ask (no pass is slowed down), but it sees only the
//! records themselves — a result's heap (an item tree's items) is counted
//! where it was allocated, by the profiling build's allocator.

use hir::RootDatabase;

/// One query's results, or one kind of interned or input value, as the
/// database keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMemory {
    /// The query's or the value's name (`infer_body`, `LibraryPackage`).
    pub name: &'static str,
    pub kind: StoredKind,
    /// How many results (kept or evicted: an evicted one keeps its
    /// metadata) or values.
    pub count: usize,
    /// The records' own size — a `Vec`'s or an `Arc`'s pointer, not what
    /// it points to.
    pub inline_bytes: usize,
    /// What Salsa keeps beside each one: the queries it read, revisions.
    pub metadata_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredKind {
    /// A query's memoized results.
    QueryResults,
    /// Interned values, inputs and tracked structs.
    Values,
}

/// Everything `db` keeps, the largest first. Waits for snapshots to be
/// dropped, as a change does.
pub(crate) fn stored_memory(db: &mut RootDatabase) -> Vec<StoredMemory> {
    let database: &mut dyn hir::salsa::Database = db;
    let usage = database.memory_usage();
    let queries = usage.queries.iter().map(|(name, info)| StoredMemory {
        name,
        kind: StoredKind::QueryResults,
        count: info.count(),
        inline_bytes: info.size_of_fields(),
        metadata_bytes: info.size_of_metadata(),
    });
    let values = usage.structs.iter().map(|info| StoredMemory {
        name: info.debug_name(),
        kind: StoredKind::Values,
        count: info.count(),
        inline_bytes: info.size_of_fields(),
        metadata_bytes: info.size_of_metadata(),
    });
    let mut stored: Vec<StoredMemory> = queries.chain(values).collect();
    stored.sort_by_key(|entry| std::cmp::Reverse(entry.inline_bytes + entry.metadata_bytes));
    stored
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnalysisHost;

    #[test]
    fn the_queries_a_file_was_analysed_with_are_counted() {
        let fixture = base_db::fixture::Fixture::parse(
            r#"
//- /app/x/A.kt
package x
fun pay(amount: Int): Int = amount + 1
"#,
        );
        let file = fixture.file("/app/x/A.kt");
        // The only handle: counting waits for every other one to go.
        let mut host = AnalysisHost {
            db: fixture.database,
        };
        let analysis = host.analysis();
        analysis
            .diagnostics(file, &crate::DiagnosticsConfig::default())
            .unwrap();
        drop(analysis);
        let stored = host.stored_memory();
        let item_trees = stored
            .iter()
            .find(|entry| entry.name == "lowered_file")
            .expect("the file's item tree was asked for");
        assert_eq!(item_trees.kind, StoredKind::QueryResults);
        assert_eq!(item_trees.count, 1);
        assert!(item_trees.inline_bytes > 0);
        let largest_first = stored.windows(2).all(|pair| {
            pair[0].inline_bytes + pair[0].metadata_bytes
                >= pair[1].inline_bytes + pair[1].metadata_bytes
        });
        assert!(largest_first);
    }
}
