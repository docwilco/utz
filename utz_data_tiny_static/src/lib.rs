//! μTZ `tiny-static` preset asset: the same decoded
//! payload as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight
//! floor 1e-3, i16, 2° grid) shipped uncompressed — ~125 KB flash, zero-copy
//! via `Finder::from_static()`, ~0 RAM, no decoder; works on the bare `core`
//! rung.
//!
//! Regenerate (writes `data/tiny-static.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz_build_cli -- gen-preset tiny-static
//! ```

#![no_std]

/// The tiny-static asset bytes (outer header + uncompressed payload).
/// 4-aligned: this preset is borrowed in place by `Finder::from_static()`, and
/// alignment keeps it valid under any geometry recipe (`FullRings` slice-casts
/// `(i32, i32)` pairs; today's varint encoding doesn't care).
pub static TINY_STATIC: &[u8] =
    include_bytes_aligned::include_bytes_aligned!(4, "../data/tiny-static.utz");
