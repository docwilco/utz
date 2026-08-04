//! μTZ `compact` preset asset: dataset `now`, RDP
//! ε=1 000 m with pop-density weight floor 1e-3, i24, 4/3° grid, xz —
//! ~445 KB flash, peak decode RAM = decoded size (~608 KB).
//!
//! Regenerate (writes `data/compact.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz_build_cli -- gen-preset compact
//! ```

#![no_std]

/// The compact asset bytes (outer header + xz payload).
pub static COMPACT: &[u8] = include_bytes!("../data/compact.utz");
