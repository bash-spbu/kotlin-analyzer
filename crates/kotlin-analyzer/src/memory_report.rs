//! `--memory`: what the process holds, by query — what Salsa keeps of each
//! (results, their metadata), and, in the profiling build
//! (`memory_profile`), the bytes each query allocated that are still live
//! and all it allocated.

use ide::{StoredKind, StoredMemory};

/// What one query allocated, as the profiling build counts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryAllocations {
    pub query: String,
    /// Allocated in the query and not freed yet — results, Salsa's records
    /// of them, and whatever else outlived it.
    pub live_bytes: i64,
    pub live_blocks: i64,
    /// Everything it allocated, freed or not.
    pub allocated_bytes: u64,
}

/// The row of what was allocated outside every query.
#[cfg_attr(not(feature = "memory-profile"), allow(dead_code))]
pub const OUTSIDE_QUERIES: &str = "(outside queries)";

/// One line per query and kind of stored value, the most live bytes
/// first; `allocations` is `None` without the profiling build.
pub fn render(stored: &[StoredMemory], allocations: Option<&[QueryAllocations]>) -> String {
    let rows = rows(stored, allocations.unwrap_or_default());
    let mut output = String::new();
    output.push_str("      live MB   blocks  allocated MB   results  salsa MB  name\n");
    let megabytes = |bytes: f64| bytes / 1_000_000.0;
    for row in &rows {
        let counted = |text: String| {
            if allocations.is_some() {
                text
            } else {
                "-".to_owned()
            }
        };
        output.push_str(&format!(
            "{:>13} {:>8} {:>13} {:>9} {:>9.1}  {}\n",
            counted(format!("{:.1}", megabytes(row.live_bytes as f64))),
            counted(row.live_blocks.to_string()),
            counted(format!("{:.0}", megabytes(row.allocated_bytes as f64))),
            row.results
                .map_or("-".to_owned(), |count| count.to_string()),
            megabytes(row.salsa_bytes as f64),
            row.name,
        ));
    }
    let live: i64 = rows.iter().map(|row| row.live_bytes).sum();
    let salsa: usize = rows.iter().map(|row| row.salsa_bytes).sum();
    let total_live = if allocations.is_some() {
        format!("{:.1}", megabytes(live as f64))
    } else {
        "-".to_owned()
    };
    output.push_str(&format!(
        "{total_live:>13} {:>8} {:>13} {:>9} {:>9.1}  total\n",
        "",
        "",
        "",
        megabytes(salsa as f64)
    ));
    if allocations.is_none() {
        output.push_str(
            "(live and allocated bytes by query: build with `--features memory-profile`)\n",
        );
    }
    output
}

struct Row {
    name: String,
    live_bytes: i64,
    live_blocks: i64,
    allocated_bytes: u64,
    /// How many results a query has (kept or evicted), or values of a kind.
    results: Option<usize>,
    /// What Salsa keeps of them: the records and their metadata.
    salsa_bytes: usize,
}

fn rows(stored: &[StoredMemory], allocations: &[QueryAllocations]) -> Vec<Row> {
    let mut rows: Vec<Row> = allocations
        .iter()
        .map(|allocation| Row {
            name: allocation.query.clone(),
            live_bytes: allocation.live_bytes,
            live_blocks: allocation.live_blocks,
            allocated_bytes: allocation.allocated_bytes,
            results: None,
            salsa_bytes: 0,
        })
        .collect();
    for entry in stored {
        let name = match entry.kind {
            StoredKind::QueryResults => entry.name.to_owned(),
            StoredKind::Values => format!("{} (values)", entry.name),
        };
        let salsa_bytes = entry.inline_bytes + entry.metadata_bytes;
        match rows.iter_mut().find(|row| row.name == name) {
            Some(row) => {
                row.results = Some(entry.count);
                row.salsa_bytes = salsa_bytes;
            }
            None => rows.push(Row {
                name,
                live_bytes: 0,
                live_blocks: 0,
                allocated_bytes: 0,
                results: Some(entry.count),
                salsa_bytes,
            }),
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse((row.live_bytes, row.salsa_bytes as i64)));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(name: &'static str, kind: StoredKind, count: usize, bytes: usize) -> StoredMemory {
        StoredMemory {
            name,
            kind,
            count,
            inline_bytes: bytes,
            metadata_bytes: bytes,
        }
    }

    #[test]
    fn a_querys_allocations_and_what_salsa_keeps_of_it_are_one_row() {
        let stored = [
            stored("infer_body", StoredKind::QueryResults, 300, 1_000_000),
            stored("lowered_file", StoredKind::QueryResults, 20, 500_000),
            stored("LibraryPackage", StoredKind::Values, 9, 2_000_000),
        ];
        let allocations = [
            QueryAllocations {
                query: OUTSIDE_QUERIES.to_owned(),
                live_bytes: 5_000_000,
                live_blocks: 10,
                allocated_bytes: 9_000_000,
            },
            QueryAllocations {
                query: "infer_body".to_owned(),
                live_bytes: 30_000_000,
                live_blocks: 1000,
                allocated_bytes: 400_000_000,
            },
        ];
        let table = render(&stored, Some(&allocations));
        let lines: Vec<&str> = table.lines().collect();
        assert!(lines[1].ends_with("infer_body"), "{table}");
        assert!(
            lines[1].contains("30.0") && lines[1].contains(" 300 ") && lines[1].contains("2.0")
        );
        assert!(lines[2].ends_with(OUTSIDE_QUERIES), "{table}");
        assert!(lines[3].ends_with("LibraryPackage (values)"), "{table}");
        assert!(lines[4].ends_with("lowered_file"), "{table}");
        assert!(
            lines[5].ends_with("total") && lines[5].contains("35.0"),
            "{table}"
        );
    }

    #[test]
    fn without_the_profiling_build_only_what_salsa_keeps_is_shown() {
        let stored = [stored(
            "infer_body",
            StoredKind::QueryResults,
            300,
            1_000_000,
        )];
        let table = render(&stored, None);
        assert!(
            table.lines().nth(1).unwrap().trim_start().starts_with("- "),
            "{table}"
        );
        assert!(table.contains("--features memory-profile"));
    }
}
