//! μTZ (micro-timezone): tiny, tunable, embeddable lat/lon → IANA timezone-id lookup.
//!
//! - **Tiny**: OSM timezone data down from 60 MB to ~70 KB via shared-arc
//!   topology, tunable map simplification, integer quantization, and general
//!   compression. Larger more accurate options available as well.
//! - **Embeddable**: pure-Rust codecs, integer point-in-polygon, flat
//!   arrays that borrow zero-copy from a flash partition. `no_std` capable.
//! - **Tunable**: pick dataset, simplification parameters, data types,
//!   quantization grid, grid cell size, and compression codec to hit your
//!   exact size / RAM / accuracy point, guided by a
//!   [visualization tool](https://docwilco.github.io/utz/live/index.html).
//!   Or use no compression for direct from flash.
//! - **DST-correct**: returns the IANA `tzid`; resolve offsets/DST
//!   downstream with [`jiff`](https://crates.io/crates/jiff) (whose
//!   compile-time static zones pair well with μTZ's embedded nature) or the
//!   prevalent `chrono-tz`.
//!
//! # Getting started
//!
//! A quick start needs just two choices, picked as cargo features: an
//! [environment](#environments) and a [preset](#preset-bundles).
//!
//! ```toml
//! [dependencies]
//! utz = { version = "0.1", features = ["std", "tiny"] }
//! ```
//!
//! A preset is a complete build: it bakes its asset into the binary and
//! enables the decoder features it needs. [`Finder::new`] loads the one
//! enabled preset:
//!
//! ```ignore
//! let finder = utz::Finder::new()?;              // or ::from_static(flash_bytes)
//! let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 });
//! // Some("Europe/London")
//! ```
//!
//! With several presets in the tree, pick explicitly via the statics in
//! the [`data`] module: `Finder::from_slice(utz::data::TINY)`
//! (compressed) or `Finder::from_static(utz::data::TINY_STATIC)`
//! (uncompressed, zero-copy). Everything beyond the presets (your own
//! simplification / quantization / codec / dataset point) goes through
//! [Building a custom dataset](#building-a-custom-dataset).
//!
//! # Preset bundles
//!
//! One Cargo feature picks a ready-made size/accuracy point; `custom`
//! instead generates your own asset with `utz-build`:
//!
//! | feature       | simplification | size    | notes |
//! |---------------|----------------|--------:|-------|
//! | `tiny`        | ε 10 km, i16   |  ~71 KB | gzip: ~125 KB RAM to decode |
//! | `tiny-static` | ε 10 km, i16   | ~125 KB | `tiny` uncompressed: zero-copy from flash, ~0 RAM, runs on bare-metal `core` |
//! | `compact`     | ε 1 km, i24    | ~445 KB | xz |
//! | `balanced`    | ε 50 m, i24    | ~1.3 MB | brotli |
//! | `accurate`    | ε 10 m, i32    | ~8.3 MB | brotli: full zone set (every distinct tzid); the others merge zones identical since now |
//!
//! Preset features are additive across the whole dependency tree, and
//! [`Finder::new`] exists only while exactly one preset is enabled:
//! with several in the union there is no single default to load, so
//! `new()` is compiled out and its call sites fail to build. Every
//! enabled preset's asset stays available as a static in [`data`];
//! load one explicitly with [`Finder::from_slice`] or
//! [`Finder::from_static`].
//!
//! # Configuring
//!
//! A build configures itself entirely through cargo features. Three
//! choices are mandatory, and forgetting one is a compile error whose
//! message explains the options: a data tier (a
//! [preset](#preset-bundles) or `custom`), an
//! [environment](#environments), and at least one
//! [geometry decoder](#geometry-decoders) (presets enable their own).
//! [Compression codecs](#compression-codecs) are additive on top. The
//! [dataset](#datasets) is a property of the asset rather than of the
//! build. The [`caps`] module exposes at compile time what a build can
//! read.
//!
//! ## Environments
//!
//! Each level adds API on top of the one below without changing it,
//! which makes the choice safe to leave to cargo's feature merging:
//! when one crate in your dependency tree asks for `core` and another
//! for `std`, the build gets `std` and both keep working.
//!
//! | feature | environment                              | can load |
//! |---------|------------------------------------------|----------|
//! | `core`  | bare metal: no allocator, ~zero heap     | uncompressed assets, zero-copy from flash |
//! | `alloc` | `no_std` plus an allocator               | compressed assets too, decoded into RAM |
//! | `std`   | full standard library (implies `alloc`)  | adds file/reader loading |
//!
//! ## Geometry decoders
//!
//! One feature per geometry encoding; a container whose encoding has no
//! compiled decoder is refused at load. Presets enable the decoder
//! their recipe uses; `custom` users pick the one(s) their assets use.
//! The measured size/speed ladder is the table on [`GeomEncoding`].
//!
//! | feature                 | decodes                             | notes |
//! |-------------------------|-------------------------------------|-------|
//! | `geom-varint-arcs`      | shared arcs, delta + zigzag varints | the preset encoding; smallest |
//! | `geom-fixed-width-arcs` | shared arcs, fixed-width coords     | faster streaming reads from flash |
//! | `geom-full-rings`       | whole rings, read in place          | fastest; little-endian hosts only |
//! | `geom-coarse`           | grid-only assets                    | cell precision; compiles no point-in-polygon code |
//!
//! ## Compression codecs
//!
//! Additive; each compiles the decoder for one payload codec.
//! Uncompressed assets need none of them. Backend crates and codec
//! bytes are in the [`decompress`] module docs.
//!
//! | feature    | codec  | environment |
//! |------------|--------|-------------|
//! | `gzip`     | gzip   | `alloc` (pure Rust) |
//! | `ruzstd`   | zstd   | `alloc` (pure Rust) |
//! | `zstd-sys` | zstd   | `std` (C libzstd; wins over `ruzstd` when both are enabled) |
//! | `brotli`   | brotli | `alloc` (pure Rust) |
//! | `xz`       | xz     | `alloc` (pure Rust) |
//!
//! ## Datasets
//!
//! The dataset is baked into an asset when it is generated (every
//! preset except `accurate` uses `now`; custom builds choose). It picks
//! the merge vintage: zones whose rules are identical from that point
//! on are merged, so older vintages keep more zones. Oceans are covered
//! by default; a `land-` prefix selects the land-only releases.
//!
//! | dataset | zones | merge |
//! |---------|------:|-------|
//! | `now`   |    65 | zones identical from today onward merged |
//! | `1970`  |   304 | zones identical since 1970 merged |
//! | `all`   |   444 | every distinct tzid kept |
//!
//! # Building a custom dataset
//!
//! The `custom` tier pairs with the `utz-build` crate. In a `build.rs`
//! (with `utz-build` as a build-dependency), the typed builder
//! ([`utz_build::Config`]) fetches the source data into a cache,
//! encodes, and writes the asset plus a guard file:
//!
//! ```ignore
//! // build.rs
//! utz_build::Config::new()
//!     .dataset("now")     // [land-]now | 1970 | all
//!     .rdp_meters(500.0)  // simplification tolerance ceiling
//!     .quant_bits(24)     // 16 / 24 / 32
//!     .codec(utz_build::Codec::Gzip)
//!     .generate()?;       // writes $OUT_DIR/tz.utz (+ .guard.rs)
//! ```
//!
//! Preset recipes double as starting points for one-knob variants:
//! `Config::tiny().codec(Codec::Uncompressed)` is exactly the
//! `tiny-static` recipe.
//!
//! The features must then match the asset: `custom`, an
//! [environment](#environments), the [geometry decoder](#geometry-decoders)
//! for its encoding (`geom-varint-arcs` for the default), and the
//! [codec feature](#compression-codecs) for its compression (none for
//! `Codec::Uncompressed`):
//!
//! ```toml
//! [dependencies]
//! utz = { version = "0.1", features = ["std", "custom", "gzip", "geom-varint-arcs"] }
//!
//! [build-dependencies]
//! utz-build = "0.1"
//! ```
//!
//! The generated guard file asserts exactly this match. Embed the asset
//! and `include!` the guard next to it, and a feature mismatch becomes
//! a compile error instead of a load error:
//!
//! ```ignore
//! include!(concat!(env!("OUT_DIR"), "/tz.utz.guard.rs"));
//! static TZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tz.utz"));
//! let finder = utz::Finder::from_slice(TZ)?;
//! ```
//!
//! Uncompressed assets can instead be borrowed zero-copy with
//! [`Finder::from_static`] (full-rings assets must be 4-byte aligned:
//! embed those with the re-exported [`include_bytes_aligned!`]). Outside
//! a `build.rs`, the `utz-build` CLI writes the same containers:
//! `utz-build gen now 500 --qbits 24 --codec gzip -o tz.utz`.
//!
//! # How it works
//!
//! ## Whittling the data down
//!
//! An asset starts as the timezone-boundary-builder `GeoJSON` (~60 MB
//! of polygons) and is reduced in stages when it is generated:
//!
//! 1. **Merge vintage**: the [dataset](#datasets) choice alone removes
//!    most zones, by merging ones whose rules are identical from the
//!    chosen point in time onward.
//! 2. **Topology**: borders shared between adjacent zones are cut into
//!    arcs at junction points and each arc is stored once; rings become
//!    lists of arc references. Roughly half to three quarters of all
//!    coordinates lie on shared borders, and this removes that
//!    duplication.
//! 3. **Simplification**: each arc is simplified once (RDP by default)
//!    to the configured tolerance, so neighboring zones stay perfectly
//!    stitched. Optional population-density weighting keeps
//!    densely-inhabited borders detailed while relaxing empty ones.
//! 4. **Quantization**: coordinates land on an integer grid at 16, 24,
//!    or 32 bits per coordinate; narrower grids mean smaller assets and
//!    narrower arithmetic at lookup time.
//! 5. **Coordinate coding**: within an arc, vertices are stored as
//!    zigzag-varint deltas, a byte or two for most steps (the other
//!    [geometry encodings](#geometry-decoders) trade that compactness
//!    for lookup speed).
//! 6. **Grid prefilter**: a coarse lon/lat grid is rasterized so most
//!    queries never touch geometry at all: an interior cell answers
//!    with its zone directly, a border cell carries a short
//!    candidate-polygon list.
//! 7. **Compression**: the section blob is compressed with the chosen
//!    codec; only the format prologue and header stay plaintext.
//!
//! ## Shipping the asset
//!
//! The result is one self-describing container (the [`format`] module
//! documents it): the header records every knob, so the decoder is
//! fully generic and one binary reads any variant handed to it. The
//! container reaches the reader either compiled in (a preset's data
//! crate, or your `build.rs` output via `include_bytes!`) or as
//! external data: a file, an OTA download, a dedicated flash
//! partition. Uncompressed containers can be used where they lie:
//! [`Finder::from_static`] borrows them zero-copy straight from
//! memory-mapped flash.
//!
//! ## Decoding and lookup
//!
//! An asset this build cannot read (a missing
//! [geometry decoder](#geometry-decoders) or
//! [codec](#compression-codecs)) is refused with a typed error before
//! any decoding starts. Decompression allocates exactly one buffer:
//! the header states the decompressed size up front. RAM use then
//! follows from how the container was loaded:
//!
//! - **zero-copy** ([`Finder::from_static`]): the container is borrowed
//!   in place and lookups stream geometry straight off the stored
//!   bytes; no heap allocation at all.
//! - **lazy** ([`Finder::from_slice`] and friends): the decompressed
//!   payload lives in owned RAM and nothing else is cached (the RAM
//!   notes in the [preset table](#preset-bundles) are this buffer).
//! - **eager** ([`Finder::preload`]): all rings are additionally
//!   decoded up front into a flat cache, the fastest mode;
//!   [`preload_bytes`] tells you the exact cost before you pay it.
//!
//! A lookup quantizes the query point and indexes the grid cell; in the
//! common case that already answers it. On a border cell it walks the
//! candidate polygons: a bounding-box gate first, then an exact integer
//! even-odd point-in-polygon test. There is no floating point in the
//! path, so results are identical on every target, and points exactly
//! on a border are claimed deterministically. [`lookup_coarse`] skips
//! geometry entirely and answers at cell precision from any asset.
//!
//! `no_std`-first: API availability follows the
//! [environment ladder](#environments) `core` ⊂ `alloc` ⊂ `std`.
//!
//! # Inspirations & credits
//!
//! μTZ stands on the shoulders of three excellent projects; it reuses
//! their ideas and pushes on size and embeddability:
//!
//! - **[spatialtime](https://github.com/moranbw/spatialtime)**: the crate
//!   μTZ grew out of. The `Reader`-style build-once/query-many API and the
//!   compression approach come from here.
//! - **[rtz](https://github.com/twitchax/rtz)**: the 1°×1° grid prefilter.
//! - **[tzf-rs](https://github.com/ringsaturn/tzf-rs)**: shared-edge
//!   (topology) boundary deduplication, the grid/preindex fast-path (its
//!   "Fuzzy" finder, μTZ's `lookup_coarse`), and delta+varint coordinate
//!   encoding.
//!
//! Where those ship fixed data tiers, μTZ makes the size/accuracy tradeoff
//! a build-time knob and adds integer quantization to go ~10× smaller,
//! with a genuinely `no_std`/flash-embeddable format.
//!
//! [`Finder::new`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.new
//! [`Finder::from_slice`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_slice
//! [`Finder::from_static`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_static
//! [`Finder::preload`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload
//! [`preload_bytes`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload_bytes
//! [`lookup_coarse`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_coarse
//! [`data`]: https://docwilco.github.io/utz/docs/utz/data/index.html
//! [`caps`]: https://docwilco.github.io/utz/docs/utz/caps/index.html
//! [`format`]: https://docwilco.github.io/utz/docs/utz/format/index.html
//! [`decompress`]: https://docwilco.github.io/utz/docs/utz/decompress/index.html
//! [`GeomEncoding`]: https://docwilco.github.io/utz/docs/utz/enum.GeomEncoding.html
//! [`include_bytes_aligned!`]: https://docwilco.github.io/utz/docs/utz/macro.include_bytes_aligned.html
//! [`utz_build::Config`]: https://docwilco.github.io/utz/docs/utz_build/struct.Config.html

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
    feature = "geom-varint-arcs",
    feature = "geom-fixed-width-arcs",
    feature = "geom-full-rings",
    feature = "geom-coarse"
)))]
compile_error!(
    "utz: pick at least one geometry decoder: `geom-varint-arcs` (what the presets \
     use — they enable it themselves), `geom-fixed-width-arcs`, `geom-full-rings`, or \
     `geom-coarse` (grid-only assets, cell precision)"
);
// FullRings reads coordinate sections as native-integer slices — LE hosts
// only. Refusing at compile time is precise here because it is an opt-in:
// big-endian builds keep every other encoding by not enabling this feature.
#[cfg(all(feature = "geom-full-rings", target_endian = "big"))]
compile_error!(
    "utz: `geom-full-rings` (FullRings) requires a little-endian host; \
     the `geom-varint-arcs`/`geom-fixed-width-arcs` encodings work on any endianness"
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
/// What an asset says about itself: which codec compressed it
/// ([`Codec`]), how its geometry is encoded ([`GeomEncoding`]), its
/// coordinate width ([`QuantBits`]), and which dataset and simplifier
/// built it ([`Dataset`], [`SimplifyAlgo`]). You meet these in the
/// parsed header ([`PayloadLayout`](format::PayloadLayout)) and in
/// errors: [`Error::CodecNotCompiledIn`] and
/// [`Error::GeometryNotCompiledIn`] carry one to name the decoder this
/// build lacks.
pub use utz_common::{Codec, Dataset, GeomEncoding, QuantBits, SimplifyAlgo};

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
    /// i16, 2° grid, gzip; ~71 K flash, peak decode RAM 125 K.
    #[cfg(feature = "tiny")]
    pub use utz_data_tiny::TINY;
    /// tiny-static preset: tiny's decoded container shipped flat; ~125 K
    /// flash, zero-copy via [`Finder::from_static`](crate::Finder::from_static),
    /// ~0 RAM, no decoder, bare-`core` capable.
    #[cfg(feature = "tiny-static")]
    pub use utz_data_tiny_static::TINY_STATIC;
}

/// Compile-time capabilities of THIS utz build (its resolved features).
///
/// For asset guards: `utz_build::Config::generate` writes a
/// `<asset>.guard.rs` next to each asset, asserting the caps it needs.
/// `include!` it beside the `include_bytes!` and a feature mismatch becomes
/// a compile error in your crate instead of a load error in the field.
/// Also useful directly for OTA/file-loaded assets:
/// `assert!(utz::caps::XZ)` at startup.
pub mod caps {
    /// delta+varint arc geometry decoder (`geom-varint-arcs`)
    pub const GEOM_VARINT_ARCS: bool = cfg!(feature = "geom-varint-arcs");
    /// fixed-width arc geometry decoder (`geom-fixed-width-arcs`)
    pub const GEOM_FIXED_WIDTH_ARCS: bool = cfg!(feature = "geom-fixed-width-arcs");
    /// `FullRings` geometry decoder (`geom-full-rings`)
    pub const GEOM_FULL_RINGS: bool = cfg!(feature = "geom-full-rings");
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
    /// best-effort: where a codec's failure status doesn't cleanly separate
    /// a short stream from a corrupt one, truncation surfaces as
    /// `DecoderFailed`.
    #[display("byte source ends before the container does (truncated)")]
    Truncated,
    /// The magic bytes don't match: not a μTZ container.
    #[display("not a μTZ container (bad magic)")]
    BadMagic,
    /// A μTZ container, but a format version this reader doesn't speak.
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
    /// The container's codec has no compiled-in backend. Enable the
    /// matching codec feature.
    #[display("codec {_0:?} has no compiled-in backend")]
    CodecNotCompiledIn(#[error(not(source))] Codec),
    /// A compiled-in codec backend rejected the stream as corrupt.
    #[cfg(feature = "alloc")]
    #[display("codec {codec:?} decoder reported: {detail}")]
    DecoderFailed {
        /// The codec whose backend failed.
        codec: Codec,
        /// The backend's own diagnostic text.
        detail: alloc::string::String,
    },
    /// Reading the byte source failed; carries the I/O error's text.
    #[cfg(feature = "std")]
    #[display("reading the container failed: {_0}")]
    ReadFailed(#[error(not(source))] alloc::string::String),
    /// [`Finder::from_static`] was handed a compressed container.
    /// Decompression needs an owned buffer (use `from_slice`/`from_vec`).
    #[display("compressed container passed to from_static")]
    StaticContainerCompressed,
    /// A `FullRings` container's coordinate section is not 4-byte aligned
    /// in memory. Embed static assets with [`include_bytes_aligned!`]`(4, ..)`
    /// instead of a bare `include_bytes!`.
    #[display("FullRings container not 4-byte aligned (use include_bytes_aligned!(4, ..))")]
    Misaligned,
    /// A `FullRings` coordinate section is misaligned within the payload
    /// itself: the container is corrupt or came from a broken encoder.
    #[display("FullRings coordinate section misaligned within the payload")]
    FullRingsSectionMisaligned,
    /// A `FullRings` container's ring-end table disagrees with its declared
    /// coordinate count.
    #[display("FullRings ring-end table disagrees with the coordinate count")]
    FullRingsCountsDisagree,
    /// The geometry encoding byte has no compiled-in decoder. Enable the
    /// matching `geom-*` feature.
    #[display(
        "geometry encoding {_0:?} has no compiled-in decoder (enable the matching geom-* feature)"
    )]
    GeometryNotCompiledIn(#[error(not(source))] GeomEncoding),
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
    /// `format_args!` over the source error: `{source}` where the backend
    /// implements `Display`, `{source:?}` otherwise (`Debug` is the one
    /// trait every backend's error type implements).
    pub(crate) fn decoder_failed(codec: Codec, detail: core::fmt::Arguments<'_>) -> Error {
        Error::DecoderFailed {
            codec,
            detail: alloc::fmt::format(detail),
        }
    }
}

/// Embed a `.utz` container with `include_bytes_aligned!(4, path)`. Required
/// for [`Finder::from_static`] on `FullRings` assets: the PIP kernels read
/// `(i32, i32)` pairs straight from the embedded bytes, and a bare
/// `include_bytes!` guarantees no alignment. Harmless for any other asset.
// Re-exported so consumers don't need their own copy of the dependency. Both
// the re-export and the dependency can go once RFC 3806's `static_align`
// stabilizes (`#[align(4)]` on a static holding `*include_bytes!(path)`):
// https://github.com/rust-lang/rfcs/pull/3806
pub use include_bytes_aligned::include_bytes_aligned;
