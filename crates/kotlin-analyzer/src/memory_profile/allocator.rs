//! The profiling build's global allocator: the system's, with the scope
//! each block was allocated in written in a header before it, so a scope's
//! bytes count as live until the block is freed — wherever that happens.
//!
//! The only code that may be `unsafe` (`cargo xtask check` holds it to
//! this file).
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};

/// Where allocations are counted: `OUTSIDE` the queries, or in one query's
/// scope.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ScopeId(pub(super) u32);

impl ScopeId {
    pub(super) const OUTSIDE: ScopeId = ScopeId(0);

    fn index(self) -> usize {
        (self.0 as usize).min(SCOPES - 1)
    }
}

/// How many scopes are counted apart; the last one takes any beyond.
pub(super) const SCOPES: usize = 256;

thread_local! {
    static CURRENT: Cell<ScopeId> = const { Cell::new(ScopeId::OUTSIDE) };
}

/// The scope this thread allocates in.
pub(super) fn current() -> ScopeId {
    CURRENT.try_with(Cell::get).unwrap_or(ScopeId::OUTSIDE)
}

pub(super) fn set_current(scope: ScopeId) {
    let _ = CURRENT.try_with(|current| current.set(scope));
}

/// What a scope allocated: still live, and in all.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct ScopeCounts {
    pub(super) live_bytes: i64,
    pub(super) live_blocks: i64,
    pub(super) allocated_bytes: u64,
}

pub(super) fn counts(scope: ScopeId) -> ScopeCounts {
    let index = scope.index();
    STRIPES
        .iter()
        .fold(ScopeCounts::default(), |total, stripe| ScopeCounts {
            live_bytes: total.live_bytes + stripe.live_bytes[index].load(Ordering::Relaxed),
            live_blocks: total.live_blocks + stripe.live_blocks[index].load(Ordering::Relaxed),
            allocated_bytes: total.allocated_bytes
                + stripe.allocated_bytes[index].load(Ordering::Relaxed),
        })
}

/// One set of counters; each thread counts in one stripe, so threads
/// don't contend for the same counters (a block freed on another thread
/// is subtracted in that thread's stripe: the sums are right).
struct Stripe {
    live_bytes: [AtomicI64; SCOPES],
    live_blocks: [AtomicI64; SCOPES],
    allocated_bytes: [AtomicU64; SCOPES],
}

const STRIPE_COUNT: usize = 16;

static STRIPES: [Stripe; STRIPE_COUNT] = [const {
    Stripe {
        live_bytes: [const { AtomicI64::new(0) }; SCOPES],
        live_blocks: [const { AtomicI64::new(0) }; SCOPES],
        allocated_bytes: [const { AtomicU64::new(0) }; SCOPES],
    }
}; STRIPE_COUNT];

static NEXT_STRIPE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// This thread's stripe, once it has allocated (`usize::MAX` before).
    static STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// The calling thread's stripe — taken without allocating, as the
/// allocator asks for it.
fn stripe() -> &'static Stripe {
    let index = STRIPE
        .try_with(|stripe| {
            if stripe.get() == usize::MAX {
                stripe.set(NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) % STRIPE_COUNT);
            }
            stripe.get()
        })
        .unwrap_or(0);
    &STRIPES[index]
}

fn count_allocated(scope: ScopeId, size: usize) {
    let index = scope.index();
    let stripe = stripe();
    stripe.live_bytes[index].fetch_add(size as i64, Ordering::Relaxed);
    stripe.live_blocks[index].fetch_add(1, Ordering::Relaxed);
    stripe.allocated_bytes[index].fetch_add(size as u64, Ordering::Relaxed);
}

fn count_freed(scope: ScopeId, size: usize) {
    let index = scope.index();
    let stripe = stripe();
    stripe.live_bytes[index].fetch_sub(size as i64, Ordering::Relaxed);
    stripe.live_blocks[index].fetch_sub(1, Ordering::Relaxed);
}

fn count_resized(scope: ScopeId, old_size: usize, new_size: usize) {
    let index = scope.index();
    let stripe = stripe();
    stripe.live_bytes[index].fetch_add(new_size as i64 - old_size as i64, Ordering::Relaxed);
    let grown = new_size.saturating_sub(old_size);
    stripe.allocated_bytes[index].fetch_add(grown as u64, Ordering::Relaxed);
}

/// The header's size for a block aligned to `align`: at least 16 bytes,
/// a multiple of the alignment, so the block after it stays aligned and
/// the scope right before the block is aligned for a `u32`.
fn header_size(align: usize) -> usize {
    align.max(16)
}

/// The layout of a block with its header, and the header's size.
fn with_header(layout: Layout) -> Option<(Layout, usize)> {
    let header = header_size(layout.align());
    let size = layout.size().checked_add(header)?;
    let full = Layout::from_size_align(size, header).ok()?;
    Some((full, header))
}

const SCOPE_BYTES: usize = std::mem::size_of::<u32>();

pub struct ScopedAllocator;

impl ScopedAllocator {
    /// `raw` allocates the block with its header; the scope goes into the
    /// header.
    ///
    /// # Safety
    /// `raw` returns null or a block of at least the layout it is given.
    unsafe fn allocate(&self, layout: Layout, raw: impl FnOnce(Layout) -> *mut u8) -> *mut u8 {
        let Some((full, header)) = with_header(layout) else {
            return std::ptr::null_mut();
        };
        let base = raw(full);
        if base.is_null() {
            return base;
        }
        let scope = current();
        // SAFETY: `base` holds `header + layout.size()` bytes, so the
        // block starts inside it, and the scope's bytes right before the
        // block are in the header; `header` is a multiple of 16, so they
        // are aligned for a `u32`.
        let block = unsafe { base.add(header) };
        unsafe { block.sub(SCOPE_BYTES).cast::<u32>().write(scope.0) };
        count_allocated(scope, layout.size());
        block
    }

    /// The scope written before `block`.
    ///
    /// # Safety
    /// `block` came from this allocator.
    unsafe fn scope_of(block: *mut u8) -> ScopeId {
        // SAFETY: `allocate` wrote the scope there.
        ScopeId(unsafe { block.sub(SCOPE_BYTES).cast::<u32>().read() })
    }
}

// SAFETY: every block is the system allocator's, with a header before it
// that `with_header` sizes the same way for a layout on every call.
unsafe impl GlobalAlloc for ScopedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the system allocator returns null or a block of `full`.
        unsafe { self.allocate(layout, |full| System.alloc(full)) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as `alloc`.
        unsafe { self.allocate(layout, |full| System.alloc_zeroed(full)) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        let Some((full, header)) = with_header(layout) else {
            return;
        };
        // SAFETY: `block` came from `alloc` with `layout`, so its header,
        // `header` bytes long, starts the system's block of `full`.
        unsafe {
            count_freed(Self::scope_of(block), layout.size());
            System.dealloc(block.sub(header), full);
        }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Some((full, header)) = with_header(layout) else {
            return std::ptr::null_mut();
        };
        let Some(new_full_size) = new_size.checked_add(header) else {
            return std::ptr::null_mut();
        };
        // SAFETY: as `dealloc`; the system keeps the header's bytes (they
        // come first) and the alignment, so the block moves with its scope.
        unsafe {
            let scope = Self::scope_of(block);
            let base = System.realloc(block.sub(header), full, new_full_size);
            if base.is_null() {
                return base;
            }
            count_resized(scope, layout.size(), new_size);
            base.add(header)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blocks_bytes_count_for_the_scope_it_was_allocated_in_until_freed() {
        let scope = ScopeId(SCOPES as u32 - 2);
        let before = counts(scope);
        set_current(scope);
        let mut block: Vec<u64> = Vec::with_capacity(1000);
        set_current(ScopeId::OUTSIDE);
        let allocated = counts(scope);
        assert_eq!(allocated.live_bytes - before.live_bytes, 8000);
        assert_eq!(allocated.live_blocks - before.live_blocks, 1);
        // Grown elsewhere, it stays its scope's.
        block.reserve_exact(2000);
        assert_eq!(
            counts(scope).live_bytes - before.live_bytes,
            8 * block.capacity() as i64
        );
        drop(block);
        let freed = counts(scope);
        assert_eq!(freed.live_bytes, before.live_bytes);
        assert_eq!(freed.live_blocks, before.live_blocks);
        assert!(freed.allocated_bytes - before.allocated_bytes >= 8000);
    }

    #[test]
    fn over_aligned_blocks_stay_aligned() {
        #[repr(align(64))]
        struct Line([u8; 64]);
        let lines: Vec<Line> = (0..3).map(|index| Line([index; 64])).collect();
        assert_eq!(lines.as_ptr() as usize % 64, 0);
        assert_eq!(lines[2].0[63], 2);
    }
}
