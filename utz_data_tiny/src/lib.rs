//! μTZ `tiny` preset asset: dataset `now`, RDP
//! ε=10 000 m with pop-density weight floor 1e-3, i16, 2° grid, gzip —
//! ~71 KB flash, peak decode RAM = decoded size (~125 KB).
//!
//! Regenerate (writes `data/tiny.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz_build_cli -- gen-preset tiny
//! ```

#![no_std]

/// The tiny asset bytes (outer header + gzip payload).
pub static TINY: &[u8] = include_bytes!("../data/tiny.utz");
