//! The library behind the `siftr` binary (`src/bin/siftr`). It exists for that binary and the
//! integration tests in `tests/`: the API carries no stability promise and may change in any release.
//!
//! One run flows through the modules in this order:
//!
//! ```text
//! observation ──▶ interpret (normalize) ──Event──▶ aggregate ──▶ analyze::Analysis
//!                                                                     │
//!                      baseline (recent runs of the same context) ◀───┘
//!                            │
//!                         signal ──▶ store
//! ```
//!
//! Every module except [`store`] is pure: no I/O. Capture and rendering live in the binary.
//! [`normalize`] is the per-line hot path and uses nothing but `std` and itself.

pub mod aggregate;
pub mod analyze;
pub mod baseline;
pub mod behavior;
pub mod context;
pub mod interpret;
pub mod normalize;
pub mod num;
pub mod observation;
pub mod signal;
pub mod store;
