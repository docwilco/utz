//! The μTZ `tiny-static` preset asset. It ships the same decoded payload
//! as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight floor
//! 1e-3, i16, 2° grid) uncompressed: it costs ~125 KB of flash, loads
//! zero-copy via `Finder::from_static()` with ~0 RAM and no decoder, and
//! works on the bare `core` rung.
//!
//! Regenerate (writes `data/tiny-static.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz_build_cli -- gen-preset tiny-static
//! ```

#![no_std]

/// The tiny-static asset bytes (outer header + uncompressed payload).
/// The bytes are 4-aligned: this preset is borrowed in place by
/// `Finder::from_static()`, and alignment keeps it valid under any geometry
/// recipe (`FullRings` slice-casts `(i32, i32)` pairs; today's varint
/// encoding doesn't care).
pub static TINY_STATIC: &[u8] =
    include_bytes_aligned::include_bytes_aligned!(4, "../data/tiny-static.utz");
