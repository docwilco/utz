//! μTZ — micro-timezone: tiny, embeddable lat/lon → IANA tzid lookup.
//!
//! Self-describing container (see PLAN.md §4) → one generic decoder: grid
//! prefilter, then per-polygon integer PIP. Three memory modes, selected by
//! how the container is loaded (§9): **zero-copy** (uncompressed asset
//! borrowed from any static source), **lazy** (payload decompressed into
//! owned RAM, no decoded-geometry cache), **eager** ([`Finder::preload`]:
//! all rings decoded up front). `no_std`-first: API availability follows
//! the environment ladder `core` ⊂ `alloc` ⊂ `std` (§11).

#![cfg_attr(not(feature = "std"), no_std)]

// §11: two mandatory, at-least-one-of feature choices. "At least one of"
// errors can only be *silenced* by feature union, never triggered — safe
// under cargo's feature unification. The message is the onboarding.
#[cfg(not(any(feature = "nano", feature = "custom")))]
compile_error!(
    "utz: pick a data tier: a preset (`nano`; `micro`/`balanced`/`accurate` to come) \
     or `custom` (bring your own asset, generated with utz-build)"
);
#[cfg(not(any(feature = "core", feature = "alloc", feature = "std")))]
compile_error!(
    "utz: choose an environment: `std`, `alloc` (no_std + allocator), \
     or `core` (bare metal: uncompressed assets only, ~zero heap)"
);

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "alloc")]
pub mod decompress;
pub mod format;
pub mod pip;

mod finder;
pub use finder::Finder;

/// Preset assets baked in by the data-tier features (§11). With exactly one
/// preset enabled, [`Finder::new`] loads it; with several in the tree, pick
/// explicitly: `Finder::from_slice(utz::data::NANO)`.
#[cfg(feature = "nano")]
pub mod data {
    /// nano preset: dataset `now`, RDP ε=10 000 m (pop-density floor 1e-4),
    /// i16, 2° grid, gzip — ~72 K flash, peak decode RAM 134 K (§14.5).
    pub use utz_data_nano::NANO;
}

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
