//! The profiling build (`--features memory-profile`): every block the
//! process allocates is counted for the query it was allocated in, at the
//! cost of a header per block and a few percent of time — not the
//! backtrace per allocation a heap profiler takes.

mod allocation_sites;
mod allocator;
mod query_scopes;

pub use allocator::ScopedAllocator;

pub use allocation_sites::sites;

use crate::memory_report::QueryAllocations;

/// Starts counting by query: from here on, a query's allocations are its
/// own. Once per process; later calls do nothing.
pub fn start() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = tracing::subscriber::set_global_default(query_scopes::QueryScopes);
    });
}

/// What each query allocated, and what was allocated outside any.
pub fn allocations() -> Vec<QueryAllocations> {
    let outside = (
        crate::memory_report::OUTSIDE_QUERIES.to_owned(),
        allocator::ScopeId::OUTSIDE,
    );
    std::iter::once(outside)
        .chain(query_scopes::scopes())
        .map(|(query, scope)| {
            let counts = allocator::counts(scope);
            QueryAllocations {
                query,
                live_bytes: counts.live_bytes,
                live_blocks: counts.live_blocks,
                allocated_bytes: counts.allocated_bytes,
                allocations: counts.allocations,
            }
        })
        .collect()
}
