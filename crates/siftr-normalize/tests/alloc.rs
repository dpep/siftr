//! Steady-state normalization must not allocate. One test per binary: the
//! counting allocator is process-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use siftr_normalize::{Normalizer, SlotKind, SlotStats};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn steady_state_does_not_allocate() {
    let lines: Vec<Vec<u8>> = [
        "\x1b[1m\x1b[36m (0.3ms)\x1b[0m  \x1b[1mINSERT INTO `schema_migrations` (version) VALUES ('20170905153814')\x1b[0m",
        "Started GET \"/users/85320\" for 10.0.24.37 at 2024-01-15 10:00:00 +0000",
        "SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18)",
        "Finished in 1 minute 3.5 seconds (files took 0.24948 seconds to load)",
    ]
    .iter()
    .map(|l| l.as_bytes().to_vec())
    .collect();

    let mut n = Normalizer::new();
    let mut stats = SlotStats::new(SlotKind::Int);
    for line in &lines {
        n.normalize(line);
    }
    for v in ["1", "2", "3"] {
        stats.observe(v.as_bytes());
    }

    let before = ALLOCS.load(Ordering::Relaxed);
    let mut slots = 0;
    for _ in 0..10_000 {
        for line in &lines {
            slots += n.normalize(line).slots.len();
        }
        for v in ["1", "2", "3"] {
            stats.observe(v.as_bytes());
        }
    }
    let after = ALLOCS.load(Ordering::Relaxed);
    assert!(slots > 0);
    assert_eq!(after - before, 0, "allocations in steady state");
}
