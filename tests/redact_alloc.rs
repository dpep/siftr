//! Redacting a clean line must not allocate: every line of a run goes through it.
//!
//! The counter is thread-local, not a process-global atomic: a `#[global_allocator]` sees every thread, so a
//! global count also charges this test for whatever the harness allocates on its own threads while the loop
//! runs. Only the thread below is measured, and between its two samples it does nothing but redact. One test
//! per binary still, since the allocator is installed process-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use siftr::normalize::secrets::{Mode, Redactor, Scanner};

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
fn clean_lines_do_not_allocate() {
    let lines: Vec<&[u8]> = [
        "  \x1b[1m\x1b[36mUser Load (0.3ms)\x1b[0m  \x1b[1m\x1b[34mSELECT \"users\".* FROM \"users\" WHERE \"users\".\"id\" = $1 LIMIT $2\x1b[0m  [[\"id\", 42], [\"LIMIT\", 1]]",
        "Started GET \"/api/v1/accounts/42?page=2\" for 127.0.0.1 at 2026-09-15 10:00:00 -0700",
        "Processing by Api::V1::AccountsController#show as HTML",
        "  Parameters: {\"id\"=>\"42\", \"password\"=>\"[FILTERED]\"}",
        "Completed 200 OK in 12ms (Views: 3.1ms | ActiveRecord: 1.2ms | Allocations: 4321)",
        "{\"event\":\"example\",\"id\":\"./spec/a_spec.rb[1:1]\",\"status\":\"passed\",\"log_offset\":1234}",
    ]
    .iter()
    .map(|l| l.as_bytes())
    .collect();

    let mut redactor = Redactor::new(Some(b"/Users/alice"));
    let mut scanner = Scanner::default();
    let mut clean = 0;
    let redactions = ROUNDS * 2 * lines.len();
    let before = ALLOCS.get();
    for _ in 0..ROUNDS {
        for mode in [Mode::Secrets, Mode::Pii] {
            for line in &lines {
                let views = redactor.line(&mut scanner, line, mode);
                clean += usize::from(views.evidence == *line);
            }
        }
    }
    let after = ALLOCS.get();
    assert_eq!(clean, redactions, "every line is clean");
    assert_eq!(
        after - before,
        0,
        "allocations across {redactions} clean line-redactions"
    );
}
