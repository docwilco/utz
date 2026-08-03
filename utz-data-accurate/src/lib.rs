//! μTZ `accurate` preset asset: dataset `all` (the
//! full Comprehensive zone set), RDP ε=10 m with pop-density weight floor
//! 1e-1, i32, 0.5° grid, brotli — ~8.1 MB flash, peak decode RAM =
//! decoded size (~10.6 MB).
//!
//! Regenerate (writes `data/accurate.utz`, gitignored) from the canonical
//! recipe; the build script refuses assets that do not match it:
//!
//! ```text
//! cargo run --release -p utz-build-cli -- gen-preset accurate
//! ```

#![no_std]

/// The accurate asset bytes (outer header + brotli payload).
pub static ACCURATE: &[u8] = include_bytes!("../data/accurate.utz");
