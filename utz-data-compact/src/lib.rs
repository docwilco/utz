//! μTZ `compact` preset asset (PLAN.md §11, §14.5): dataset `now`, RDP
//! ε=1 000 m with pop-density weight floor 1e-3, i24, 4/3° grid, xz.
//!
//! Regenerate (writes `data/compact.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen now 1000 --qbits 24 \
//!     --w-min 0.001 --grid-deg 1.3333333333333333 --codec xz \
//!     -o utz-data-compact/data/compact.utz
//! ```

#![no_std]

/// The compact container bytes (outer header + xz payload).
pub static COMPACT: &[u8] = include_bytes!("../data/compact.utz");
