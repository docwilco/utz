//! `μTZ` — micro-timezone: tiny, embeddable lat/lon → IANA tzid lookup.
//!
//! Self-describing container (see [`format`](mod@format)) → one generic decoder: grid
//! prefilter, then per-polygon integer PIP. Three memory modes, selected by
//! how the container is loaded: **zero-copy** (uncompressed asset
//! borrowed from any static source), **lazy** (payload decompressed into
//! owned RAM, no decoded-geometry cache), **eager** ([`Finder::preload`]:
//! all rings decoded up front). `no_std`-first: API availability follows
//! the environment ladder `core` ⊂ `alloc` ⊂ `std`.

#![cfg_attr(not(feature = "std"), no_std)]

// Three mandatory, at-least-one-of feature choices. "At least one of"
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
#[cfg(not(any(
    feature = "geom-varint",
    feature = "geom-fixed",
    feature = "geom-image",
    feature = "geom-coarse"
)))]
compile_error!(
    "utz: pick at least one geometry decoder: `geom-varint` (what the presets \
     use — they enable it themselves), `geom-fixed`, `geom-image`, or \
     `geom-coarse` (grid-only assets, cell precision)"
);
// EagerImage reads coordinate sections as native-integer slices — LE hosts
// only. Refusing at compile time is precise here because it is an opt-in:
// big-endian builds keep every other encoding by not enabling this feature.
#[cfg(all(feature = "geom-image", target_endian = "big"))]
compile_error!(
    "utz: `geom-image` (EagerImage) requires a little-endian host; \
     the `geom-varint`/`geom-fixed` encodings work on any endianness"
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
/// The shared container vocabulary: the payload codec identifiers
/// ([`decompress`](decompress::decompress) dispatches on them), the
/// geometry/quantization/provenance types the parsed
/// [`PayloadLayout`](format::PayloadLayout) carries.
pub use utz_common::{Codec, Dataset, GeomEncoding, QuantBits, SimplifyAlgo, Vintage};

/// Preset assets baked in by the data-tier features. With exactly one
/// preset enabled, `Finder::new` loads it; with several in the tree, pick
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
    /// accurate preset: dataset `all` (every distinct tzid), RDP ε=10 m
    /// (pop-density floor 1e-1), i32, 0.5° grid, brotli.
    #[cfg(feature = "accurate")]
    pub use utz_data_accurate::ACCURATE;
    /// balanced preset: dataset `now`, RDP ε=50 m (pop-density floor 2e-2),
    /// i24, 2/3° grid, brotli.
    #[cfg(feature = "balanced")]
    pub use utz_data_balanced::BALANCED;
    /// compact preset: dataset `now`, RDP ε=1 000 m (pop-density floor 1e-3),
    /// i24, 4/3° grid, xz.
    #[cfg(feature = "compact")]
    pub use utz_data_compact::COMPACT;
    /// tiny preset: dataset `now`, RDP ε=10 000 m (pop-density floor 1e-3),
    /// i16, 2° grid, gzip — ~71 K flash, peak decode RAM 125 K.
    #[cfg(feature = "tiny")]
    pub use utz_data_tiny::TINY;
    /// tiny-static preset: tiny's decoded container shipped flat — ~125 K
    /// flash, zero-copy via [`Finder::from_static`](crate::Finder::from_static),
    /// ~0 RAM, no decoder, bare-`core` capable.
    #[cfg(feature = "tiny-static")]
    pub use utz_data_tiny_static::TINY_STATIC;
}

/// Compile-time capabilities of THIS utz build (its resolved features).
///
/// For asset guards: `utz_build::Config::generate` writes a
/// `<asset>.guard.rs` next to each asset, asserting the caps it needs —
/// `include!` it beside the `include_bytes!` and a feature mismatch becomes
/// a compile error in your crate instead of a load error in the field.
/// Also useful directly for OTA/file-loaded assets:
/// `assert!(utz::caps::XZ)` at startup.
pub mod caps {
    /// delta+varint arc geometry decoder (`geom-varint`)
    pub const GEOM_VARINT: bool = cfg!(feature = "geom-varint");
    /// fixed-width arc geometry decoder (`geom-fixed`)
    pub const GEOM_FIXED: bool = cfg!(feature = "geom-fixed");
    /// `EagerImage` geometry decoder (`geom-image`)
    pub const GEOM_IMAGE: bool = cfg!(feature = "geom-image");
    /// grid-only coarse assets (`geom-coarse`)
    pub const GEOM_COARSE: bool = cfg!(feature = "geom-coarse");
    /// gzip payload decoder (`gzip`)
    pub const GZIP: bool = cfg!(feature = "gzip");
    /// zstd payload decoder (either backend: `ruzstd` / `zstd-sys`)
    pub const ZSTD: bool = cfg!(any(feature = "ruzstd", feature = "zstd-sys"));
    /// brotli payload decoder (`brotli`)
    pub const BROTLI: bool = cfg!(feature = "brotli");
    /// xz payload decoder (`xz`)
    pub const XZ: bool = cfg!(feature = "xz");
}

/// Errors surfaced by the reader.
///
/// Variants carrying captured text (`DecoderFailed`, `ReadFailed`) exist only
/// on the feature rungs whose code can produce them; the text is best-effort
/// diagnostics for logs, not a parseable API.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display, derive_more::Error)]
pub enum Error {
    /// The byte source ends before the container does: shorter than the
    /// outer header, or a payload cut short. Payload truncation is detected
    /// best-effort — where a codec's failure status doesn't cleanly separate
    /// a short stream from a corrupt one, truncation surfaces as
    /// [`DecoderFailed`](Error::DecoderFailed).
    #[display("byte source ends before the container does (truncated)")]
    Truncated,
    /// The magic bytes don't match — not a `μTZ` container.
    #[display("not a μTZ container (bad magic)")]
    BadMagic,
    /// A `μTZ` container, but a format version this reader doesn't speak.
    #[display("unsupported container version {_0}")]
    UnsupportedVersion(#[error(not(source))] u8),
    /// A payload header field holds an invalid value (quantization bits,
    /// geometry byte, flags, or grid degrees).
    #[display("invalid payload header field")]
    InvalidHeaderField,
    /// The payload is too short for its header, or a section overruns it.
    #[display("section overruns the payload")]
    SectionOverrun,
    /// The decoded payload size disagrees with the outer header's raw length.
    #[display("decoded size disagrees with the header's raw length")]
    RawLengthMismatch,
    /// The container's codec byte has no compiled-in backend — enable the
    /// matching codec feature.
    #[display("codec {_0} has no compiled-in backend")]
    CodecNotCompiledIn(#[error(not(source))] u8),
    /// A compiled-in codec backend rejected the stream as corrupt.
    #[cfg(feature = "alloc")]
    #[display("codec {codec} decoder reported: {detail}")]
    DecoderFailed {
        /// Codec byte of the backend that failed.
        codec: u8,
        /// The backend's own diagnostic text.
        detail: alloc::string::String,
    },
    /// Reading the byte source failed; carries the I/O error's text.
    #[cfg(feature = "std")]
    #[display("reading the container failed: {_0}")]
    ReadFailed(#[error(not(source))] alloc::string::String),
    /// [`Finder::from_static`] was handed a compressed container —
    /// decompression needs an owned buffer (use `from_slice`/`from_vec`).
    #[display("compressed container passed to from_static")]
    StaticContainerCompressed,
    /// An `EagerImage` container's coordinate section is not 4-byte aligned
    /// in memory — embed static assets with [`include_bytes_aligned!`]`(4, ..)`
    /// instead of a bare `include_bytes!`.
    #[display("EagerImage container not 4-byte aligned (use include_bytes_aligned!(4, ..))")]
    Misaligned,
    /// An `EagerImage` coordinate section is misaligned within the payload
    /// itself — the container is corrupt or came from a broken encoder.
    #[display("EagerImage coordinate section misaligned within the payload")]
    ImageSectionMisaligned,
    /// An `EagerImage` container's ring-end table disagrees with its declared
    /// coordinate count.
    #[display("EagerImage ring-end table disagrees with the coordinate count")]
    ImageCountsDisagree,
    /// The geometry encoding byte has no compiled-in decoder — enable the
    /// matching `geom-*` feature.
    #[display(
        "geometry encoding {_0} has no compiled-in decoder (enable the matching geom-* feature)"
    )]
    GeometryNotCompiledIn(#[error(not(source))] u8),
}

/// Shorthand for `Result` with this crate's [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(any(
    feature = "gzip",
    feature = "ruzstd",
    feature = "zstd-sys",
    feature = "brotli",
    feature = "xz"
))]
impl Error {
    /// A `DecoderFailed` capturing the backend's diagnostic. Callers pass
    /// `format_args!` over the source error — `{source}` where the backend
    /// implements `Display`, `{source:?}` otherwise (`Debug` is the one
    /// trait every backend's error type implements).
    pub(crate) fn decoder_failed(codec: u8, detail: core::fmt::Arguments<'_>) -> Error {
        Error::DecoderFailed {
            codec,
            detail: alloc::fmt::format(detail),
        }
    }
}

/// Embed a `.utz` container with `include_bytes_aligned!(4, path)`. Required
/// for [`Finder::from_static`] on `EagerImage` assets — the PIP kernels read
/// `(i32, i32)` pairs straight from the embedded bytes, and a bare
/// `include_bytes!` guarantees no alignment. Harmless for any other asset.
// Re-exported so consumers don't need their own copy of the dependency. Both
// the re-export and the dependency can go once RFC 3806's `static_align`
// stabilizes (`#[align(4)]` on a static holding `*include_bytes!(path)`):
// https://github.com/rust-lang/rfcs/pull/3806
pub use include_bytes_aligned::include_bytes_aligned;
