//! μTZ `balanced` preset asset: dataset `now`, RDP
//! ε=50 m with pop-density weight floor 2e-2, i24, 2/3° grid, brotli —
//! ~1.2 MB flash, peak decode RAM = decoded size (~1.9 MB).
//!
//! Regenerate (writes `data/balanced.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz-build-cli -- gen-preset balanced
//! ```

#![no_std]

/// The balanced asset bytes (outer header + brotli payload).
pub static BALANCED: &[u8] = include_bytes!("../data/balanced.utz");
