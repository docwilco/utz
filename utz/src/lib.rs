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
#[cfg(not(any(
    feature = "tiny",
    feature = "tiny-static",
    feature = "compact",
    feature = "balanced",
    feature = "accurate",
    feature = "custom"
)))]
compile_error!(
    "utz: pick a data tier: a preset (`tiny`/`tiny-static`/`compact`/`balanced`/`accurate`) \
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
pub use finder::{Finder, Position};

/// Preset assets baked in by the data-tier features (§11). With exactly one
/// preset enabled, [`Finder::new`] loads it; with several in the tree, pick
/// explicitly: `Finder::from_slice(utz::data::TINY)` /
/// `Finder::from_static(utz::data::TINY_STATIC)`.
#[cfg(any(
    feature = "tiny",
    feature = "tiny-static",
    feature = "compact",
    feature = "balanced",
    feature = "accurate"
))]
pub mod data {
    /// tiny preset: dataset `now`, RDP ε=10 000 m (pop-density floor 1e-3),
    /// i16, 2° grid, gzip — ~67 K flash, peak decode RAM 119 K (§14.5).
    #[cfg(feature = "tiny")]
    pub use utz_data_tiny::TINY;
    /// tiny-static preset: tiny's decoded container shipped flat — ~119 K
    /// flash, zero-copy via [`Finder::from_static`](crate::Finder::from_static),
    /// ~0 RAM, no decoder, bare-`core` capable (§14.5).
    #[cfg(feature = "tiny-static")]
    pub use utz_data_tiny_static::TINY_STATIC;
    /// compact preset: dataset `now`, RDP ε=1 000 m (pop-density floor 1e-3),
    /// i24, 4/3° grid, xz (§14.5).
    #[cfg(feature = "compact")]
    pub use utz_data_compact::COMPACT;
    /// balanced preset: dataset `now`, RDP ε=50 m (pop-density floor 2e-2),
    /// i24, 2/3° grid, brotli (§14.5).
    #[cfg(feature = "balanced")]
    pub use utz_data_balanced::BALANCED;
    /// accurate preset: dataset `now`, RDP ε=10 m (pop-density floor 1e-1),
    /// i32, 0.5° grid, brotli (§14.5).
    #[cfg(feature = "accurate")]
    pub use utz_data_accurate::ACCURATE;
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
