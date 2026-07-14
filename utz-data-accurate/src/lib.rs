//! `μTZ` `accurate` preset asset (PLAN.md §11, §14.5): dataset `all` (the
//! full Comprehensive zone set), RDP ε=10 m with pop-density weight floor
//! 1e-1, i32, 0.5° grid, brotli.
//!
//! Regenerate (writes `data/accurate.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen all 10 --qbits 32 \
//!     --w-min 0.10 --grid-deg 0.5 --codec brotli \
//!     -o utz-data-accurate/data/accurate.utz
//! ```

#![no_std]

/// The accurate container bytes (outer header + brotli payload).
pub static ACCURATE: &[u8] = include_bytes!("../data/accurate.utz");
