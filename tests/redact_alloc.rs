//! Redacting a clean line must not allocate: every line of a run goes through it. One test per binary: the
//! counting allocator is process-global.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use siftr::normalize::secrets::{Mode, Redactor, Scanner};

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
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..10_000 {
        for mode in [Mode::Secrets, Mode::Pii] {
            for line in &lines {
                let views = redactor.line(&mut scanner, line, mode);
                clean += usize::from(views.evidence == *line);
            }
        }
    }
    let after = ALLOCS.load(Ordering::Relaxed);
    assert_eq!(clean, 10_000 * 2 * lines.len(), "every line is clean");
    assert_eq!(after - before, 0, "allocations on clean lines");
}
