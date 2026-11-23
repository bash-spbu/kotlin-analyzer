//! Where allocations come from, by sampling: one allocator call in
//! [`INTERVAL`] records its stack (return addresses only — names are looked
//! up once, at the end), so finding the code that allocates costs a little,
//! not a backtrace per allocation.

use std::cell::Cell;
use std::sync::Mutex;

use rustc_hash::FxHashMap;

use crate::memory_report::AllocationSite;

/// One allocator call in this many is sampled.
pub(super) const INTERVAL: u32 = 4096;

/// How many frames of a stack are kept: enough to get past the allocator,
/// the collections and generic helpers to the code that asked.
const DEPTH: usize = 48;

thread_local! {
    /// Calls until this thread's next sample.
    static UNTIL_SAMPLE: Cell<u32> = const { Cell::new(INTERVAL) };
    /// Set while a sample is being taken: what it allocates isn't sampled.
    static SAMPLING: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default)]
struct Sampled {
    samples: u64,
    bytes: u64,
}

static SAMPLES: Mutex<Option<FxHashMap<Vec<usize>, Sampled>>> = Mutex::new(None);

/// Counts an allocator call of `size` bytes; every [`INTERVAL`]th records
/// its stack.
pub(super) fn sample(size: usize) {
    let due = UNTIL_SAMPLE
        .try_with(|until| {
            let left = until.get().saturating_sub(1);
            let due = left == 0;
            until.set(if due { INTERVAL } else { left });
            due
        })
        .unwrap_or(false);
    if !due {
        return;
    }
    let already_sampling = SAMPLING
        .try_with(|sampling| sampling.replace(true))
        .unwrap_or(true);
    if already_sampling {
        return;
    }
    let mut addresses = [0usize; DEPTH];
    let mut depth = 0;
    backtrace::trace(|frame| {
        addresses[depth] = frame.ip() as usize;
        depth += 1;
        depth < DEPTH
    });
    {
        let mut samples = SAMPLES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sampled = samples
            .get_or_insert_with(FxHashMap::default)
            .entry(addresses[..depth].to_vec())
            .or_default();
        sampled.samples += 1;
        sampled.bytes += size as u64;
    }
    let _ = SAMPLING.try_with(|sampling| sampling.set(false));
}

/// Each place in the workspace's code that allocated, with what it called
/// to (`Vec::push`, a `clone`), estimated from the samples; the most first.
pub fn sites() -> Vec<AllocationSite> {
    let _ = SAMPLING.try_with(|sampling| sampling.set(true));
    let samples = SAMPLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .unwrap_or_default();
    let mut frames_at: FxHashMap<usize, Vec<Frame>> = FxHashMap::default();
    let mut by_site: FxHashMap<SiteKey, (u64, u64)> = FxHashMap::default();
    for (stack, sampled) in samples {
        for address in &stack {
            frames_at
                .entry(*address)
                .or_insert_with(|| frames_of(*address));
        }
        let frames: Vec<&Frame> = stack
            .iter()
            .flat_map(|address| frames_at.get(address).into_iter().flatten())
            .collect();
        let key = site_of(&frames);
        let entry = by_site.entry(key).or_default();
        entry.0 += sampled.samples;
        entry.1 += sampled.bytes;
    }
    let _ = SAMPLING.try_with(|sampling| sampling.set(false));
    let mut sites: Vec<AllocationSite> = by_site
        .into_iter()
        .map(|(key, (samples, bytes))| AllocationSite {
            site: key.site,
            via: key.via,
            caller: key.caller,
            allocations: samples * u64::from(INTERVAL),
            bytes: bytes * u64::from(INTERVAL),
        })
        .collect();
    sites.sort_by_key(|site| std::cmp::Reverse(site.allocations));
    sites
}

/// One function in a stack — inlined ones are frames too.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    /// Without its hash; an inlined function's may lack its path.
    name: String,
    /// Where in the workspace's sources (`hir-ty/src/infer/call.rs:120`),
    /// `None` for other code.
    own_place: Option<String>,
}

/// The functions at `address`, the innermost inlined one first.
fn frames_of(address: usize) -> Vec<Frame> {
    let mut frames = Vec::new();
    backtrace::resolve(address as *mut std::ffi::c_void, |symbol| {
        let name = symbol
            .name()
            .map_or("?".to_owned(), |name| format!("{name:#}"));
        let own_place = symbol
            .filename()
            .and_then(own_file)
            .filter(|file| !is_profiler(file, &name))
            .map(|file| format!("{file}:{}", symbol.lineno().unwrap_or(0)));
        frames.push(Frame { name, own_place });
    });
    frames
}

/// `file` relative to the workspace's `crates/` when it is the workspace's
/// code.
fn own_file(file: &std::path::Path) -> Option<String> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
    let relative = file.strip_prefix(crates).ok()?;
    Some(relative.to_string_lossy().into_owned())
}

/// Whether the function `name` in `file` is the allocator's or the
/// sampler's — never a site (the sampler's tests are).
fn is_profiler(file: &str, name: &str) -> bool {
    let in_allocator = file.ends_with("memory_profile/allocator.rs");
    let in_sampler = file.ends_with("memory_profile/allocation_sites.rs");
    in_allocator || (in_sampler && !name.contains("tests"))
}

/// The innermost frame of the workspace's code in `frames` (innermost
/// first), as `function (file:line)`; the frame it called (what allocated
/// for it: `Vec::push`, `String::clone`); and the next frame of the
/// workspace's code outside it, in another function (who asked, when the
/// site is a helper).
fn site_of(frames: &[&Frame]) -> SiteKey {
    let Some(own) = frames.iter().position(|frame| frame.own_place.is_some()) else {
        let outermost = frames.last().map_or("?", |frame| frame.name.as_str());
        return SiteKey {
            site: format!("(outside the workspace) {outermost}"),
            via: String::new(),
            caller: String::new(),
        };
    };
    let site = frames[own];
    let called = own
        .checked_sub(1)
        .and_then(|callee| frames.get(callee))
        .map_or(String::new(), |frame| frame.name.clone());
    let caller = frames[own + 1..]
        .iter()
        .find(|frame| frame.own_place.is_some() && frame.name != site.name)
        .map_or(String::new(), |frame| described(frame));
    SiteKey {
        site: described(site),
        via: called,
        caller,
    }
}

/// `function (file:line)`.
fn described(frame: &Frame) -> String {
    let place = frame.own_place.as_deref().unwrap_or("?");
    format!("{} ({place})", frame.name)
}

/// What samples are grouped by.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct SiteKey {
    site: String,
    via: String,
    caller: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_site_is_the_innermost_workspace_frame_and_what_it_called() {
        let frame = |name: &str, own_place: Option<&str>| Frame {
            name: name.to_owned(),
            own_place: own_place.map(str::to_owned),
        };
        let frames = [
            frame("__rustc::__rust_alloc", None),
            frame("grow_one<T>", None),
            frame("alloc::vec::Vec<T,A>::push", None),
            frame("simple_call", Some("hir-ty/src/infer/call.rs:120")),
            frame(
                "hir_ty::infer::infer_body",
                Some("hir-ty/src/infer/units.rs:180"),
            ),
        ];
        let frames: Vec<&Frame> = frames.iter().collect();
        assert_eq!(
            site_of(&frames),
            SiteKey {
                site: "simple_call (hir-ty/src/infer/call.rs:120)".to_owned(),
                via: "alloc::vec::Vec<T,A>::push".to_owned(),
                caller: "hir_ty::infer::infer_body (hir-ty/src/infer/units.rs:180)".to_owned(),
            }
        );
        let outside = [frame("__rustc::__rust_alloc", None), frame("start", None)];
        let outside: Vec<&Frame> = outside.iter().collect();
        assert_eq!(site_of(&outside).site, "(outside the workspace) start");
    }

    #[test]
    fn workspace_files_are_own_and_the_profilers_frames_never_a_site() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        assert_eq!(
            own_file(&crates.join("hir-ty/src/infer/call.rs")).as_deref(),
            Some("hir-ty/src/infer/call.rs")
        );
        assert!(is_profiler(
            "kotlin-analyzer/src/memory_profile/allocator.rs",
            "count_allocated"
        ));
        assert!(is_profiler(
            "kotlin-analyzer/src/memory_profile/allocation_sites.rs",
            "sample"
        ));
        assert!(!is_profiler(
            "kotlin-analyzer/src/memory_profile/allocation_sites.rs",
            "kotlin_analyzer::memory_profile::allocation_sites::tests::a_test"
        ));
        assert_eq!(
            own_file(std::path::Path::new("/rustc/library/alloc/src/vec/mod.rs")),
            None
        );
    }

    #[test]
    fn every_interval_th_call_is_sampled_with_its_stack() {
        for _ in 0..INTERVAL * 2 {
            sample(8);
        }
        let sites = sites();
        let sampled: u64 = sites.iter().map(|site| site.allocations).sum();
        assert!(sampled >= u64::from(INTERVAL) * 2, "{sampled}");
        assert!(
            sites
                .iter()
                .any(|site| site.site.contains("every_interval_th_call_is_sampled")),
            "{:?}",
            sites.iter().map(|site| &site.site).collect::<Vec<_>>()
        );
    }
}
