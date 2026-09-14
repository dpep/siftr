//! Siftr's pure domain. No I/O: capture, storage and rendering live in other crates.
//!
//! One run flows through the modules in this order:
//!
//! ```text
//! observation ──▶ interpret (normalize) ──Event──▶ aggregate ──▶ analyze::Analysis
//!                                                                     │
//!                      baseline (recent runs of the same context) ◀───┘
//!                            │
//!                         signal
//! ```

pub mod aggregate;
pub mod analyze;
pub mod baseline;
pub mod behavior;
pub mod context;
pub mod interpret;
pub use siftr_normalize as normalize;
pub mod num;
pub mod observation;
pub mod signal;
