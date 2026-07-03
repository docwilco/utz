//! μTZ — micro-timezone: tiny, embeddable lat/lon → IANA tzid lookup.
//!
//! Self-describing container (see PLAN.md §4) → one generic decoder: grid
//! prefilter, lazy per-polygon integer PIP. `no_std` + `alloc`.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod decompress;
pub mod format;
pub mod pip;

mod finder;
pub use finder::Finder;

/// Errors surfaced by the reader.
#[derive(Debug, PartialEq, derive_more::Display, derive_more::Error)]
pub enum Error {
    /// The byte source is not a valid μTZ container.
    #[display("not a valid μTZ container")]
    BadFormat,
    /// Container is compressed with a codec this build cannot decode
    /// (or `from_static` was handed a non-`uncompressed` container).
    #[display("codec not compiled in, or decompression failed")]
    Decompress,
}

// Finder::new() (embedded asset via `embed` feature) lands with build.rs
// (PLAN.md §5); until then load via from_static / from_reader.
