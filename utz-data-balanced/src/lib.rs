//! μTZ `balanced` preset asset (PLAN.md §11, §14.5): dataset `now`, RDP
//! ε=50 m with pop-density weight floor 2e-2, i24, 2/3° grid, brotli.
//!
//! Regenerate (writes `data/balanced.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen now 50 --qbits 24 \
//!     --w-min 0.020 --grid-deg 0.6666666666666666 --codec brotli \
//!     -o utz-data-balanced/data/balanced.utz
//! ```

#![no_std]

/// The balanced container bytes (outer header + brotli payload).
pub static BALANCED: &[u8] = include_bytes!("../data/balanced.utz");
