//! `μTZ` `tiny-static` preset asset (PLAN.md §11, §14.5): the SAME decoded
//! container as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight
//! floor 1e-3, i16, 2° grid) shipped uncompressed — ~119 K flash, zero-copy
//! via `Finder::from_static`, ~0 RAM, no decoder; works on the bare `core`
//! rung.
//!
//! Regenerate (writes `data/tiny-static.utz`, gitignored):
//!
//! ```text
//! cargo run --release -p utz-build -- gen now 10000 --qbits 16 \
//!     --w-min 0.001 --codec none -o utz-data-tiny-static/data/tiny-static.utz
//! ```

#![no_std]

/// The tiny-static container bytes (outer header + uncompressed payload).
/// 4-aligned: this preset is borrowed in place by `Finder::from_static`, and
/// alignment keeps it valid under any geometry recipe (`EagerImage` slice-casts
/// `(i32, i32)` pairs; today's varint encoding doesn't care).
pub static TINY_STATIC: &[u8] =
    include_bytes_aligned::include_bytes_aligned!(4, "../data/tiny-static.utz");
