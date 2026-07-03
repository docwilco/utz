//! uTZ — micro-timezone: tiny, embeddable lat/lon → IANA tzid lookup.
//!
//! SKELETON. The engine (self-describing container decode, ndarray grid, integer
//! per-polygon PIP) is being implemented per ../PLAN.md. The public shape below is
//! the agreed contract; bodies are stubs.

#![cfg_attr(not(feature = "std"), no_std)]

use core::result::Result;

/// Errors surfaced by the reader. (Variants firmed up during implementation.)
#[derive(Debug)]
pub enum Error {
    /// The byte source is not a valid uTZ container.
    BadFormat,
    /// Decompression of the embedded/loaded asset failed.
    Decompress,
    /// No timezone covers the point (should not happen with with-oceans data).
    NoCoverage,
}

/// A loaded timezone index. Build once, query many (see PLAN.md §3).
pub struct Finder {
    // header + zone table + arc store + ring index + grid live here.
    _private: (),
}

impl Finder {
    /// Load the asset embedded at build time (requires the `embed` feature).
    pub fn new() -> Result<Finder, Error> {
        todo!("decode EMBEDDED asset via from_static")
    }

    /// Borrow a container from `&'static` bytes (e.g. a flash partition).
    /// Zero-copy in `uncompressed` mode.
    pub fn from_static(_bytes: &'static [u8]) -> Result<Finder, Error> {
        todo!("parse self-describing header; borrow arcs/grid where possible")
    }

    /// Accurate lookup: grid prefilter → per-polygon integer PIP on candidates.
    /// Returns the IANA tzid (resolve DST downstream, e.g. chrono-tz).
    pub fn lookup(&self, _lon: f64, _lat: f64) -> Option<&str> {
        todo!("grid cell → interior zone (O(1)) or border candidates → PIP")
    }

    /// Grid-only approximate lookup: no geometry decoded, ~cell-size border error.
    pub fn fuzzy(&self, _lon: f64, _lat: f64) -> Option<&str> {
        todo!("grid cell → dominant/only zone")
    }
}
