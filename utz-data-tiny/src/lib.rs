//! `μTZ` `tiny` preset asset (PLAN.md §11, §14.5): dataset `now`, RDP
//! ε=10 000 m with pop-density weight floor 1e-3, i16, 2° grid, gzip —
//! ~71 K flash, peak decode RAM = decoded size (125 K).
//!
//! Regenerate (writes `data/tiny.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen now 10000 --qbits 16 \
//!     --w-min 0.001 --codec gzip -o utz-data-tiny/data/tiny.utz
//! ```

#![no_std]

/// The tiny container bytes (outer header + gzip payload).
pub static TINY: &[u8] = include_bytes!("../data/tiny.utz");
