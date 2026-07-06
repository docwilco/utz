//! μTZ `nano` preset asset (PLAN.md §11, §14.5): dataset `now`, RDP
//! ε=10 000 m with pop-density weight floor 1e-4, i16, 2° grid, gzip —
//! ~72 K flash, peak decode RAM = decoded size (134 K).
//!
//! Regenerate (writes `data/nano.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen now 10000 --qbits 16 \
//!     --w-min 0.0001 --codec gzip -o utz-data-nano/data/nano.utz
//! ```

#![no_std]

/// The nano container bytes (outer header + gzip payload).
pub static NANO: &[u8] = include_bytes!("../data/nano.utz");
