//! Steady-state normalization must not allocate.
//!
//! The counter is thread-local, not a process-global atomic: a `#[global_allocator]` sees every thread, so a
//! global count also charges this test for whatever the harness allocates on its own threads while the loop
//! runs. Only the thread below is measured, and between its two samples it does nothing but normalize. One
//! test per binary still, since the allocator is installed process-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use siftr::normalize::{Normalizer, Roots, SlotKind, SlotStats};

struct Counting;

thread_local! {
    /// Const-initialized and not `Drop`, so reading it from inside the allocator neither allocates nor
    /// re-enters it.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn counted() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        counted();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        counted();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const ROUNDS: usize = 10_000;

#[test]
fn steady_state_does_not_allocate() {
    let lines: Vec<Vec<u8>> = [
        "\x1b[1m\x1b[36m (0.3ms)\x1b[0m  \x1b[1mINSERT INTO `schema_migrations` (version) VALUES ('20170905153814')\x1b[0m",
        "Started GET \"/users/85320\" for 10.0.24.37 at 2024-01-15 10:00:00 +0000",
        "SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18)",
        "Finished in 1 minute 3.5 seconds (files took 0.24948 seconds to load)",
        "DEPRECATION WARNING: old (called from /Users/alice/code/app/app/views/users/show.html.erb:12)",
        "Wrote /var/folders/yr/gngx90zx/T/d20260915-4821-abc/cache.bin",
        "/Users/alice/.rvm/gems/ruby-3.4.9/gems/activerecord-7.1.3/lib/active_record/base.rb:42:in 'find'",
        "could not obtain lock on tmp/pids/server.pid",
    ]
    .iter()
    .map(|l| l.as_bytes().to_vec())
    .collect();

    let mut n = Normalizer::with_roots(
        Roots::default()
            .project("/Users/alice/code/app")
            .home("/Users/alice")
            .tmp("/tmp"),
    );
    let mut stats = SlotStats::new(SlotKind::Int);
    for line in &lines {
        n.normalize(line);
    }
    for v in ["1", "2", "3"] {
        stats.observe(v.as_bytes());
    }

    let before = ALLOCS.get();
    let mut slots = 0;
    for _ in 0..ROUNDS {
        for line in &lines {
            slots += n.normalize(line).slots.len();
        }
        for v in ["1", "2", "3"] {
            stats.observe(v.as_bytes());
        }
    }
    let after = ALLOCS.get();
    assert!(slots > 0);
    assert_eq!(
        after - before,
        0,
        "allocations across {} steady-state normalizations",
        ROUNDS * lines.len()
    );
}
