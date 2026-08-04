# μTZ

[![crates.io](https://img.shields.io/crates/v/utz.svg)](https://crates.io/crates/utz)
[![docs.rs](https://docs.rs/utz/badge.svg)](https://docs.rs/utz)
[![docs](https://img.shields.io/badge/docs-github.io-blue)](https://docwilco.github.io/utz/docs/utz/)
[![CI](https://github.com/docwilco/utz/actions/workflows/ci.yml/badge.svg)](https://github.com/docwilco/utz/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/crates/msrv/utz.svg)](https://crates.io/crates/utz)
[![license](https://img.shields.io/crates/l/utz.svg)](https://github.com/docwilco/utz/blob/main/LICENSE)

μTZ (micro-timezone): tiny, tunable, embeddable lat/lon → IANA timezone-id
lookup.

- **Tiny**: shared-arc topology, tunable map simplification, integer
  quantization, and general compression take [OpenStreetMap] timezone
  data down from ~80 MB to ~71 KB. Larger, more accurate options are
  available as well.
- **Embeddable**: the codecs are pure Rust, the point-in-polygon test is
  integer-only, and the crate is `no_std` capable.
- **Tunable**: pick dataset, simplification parameters, data types,
  quantization grid, grid cell size, and compression codec to hit your exact
  size / RAM / accuracy point, guided by a [visualization
  tool](https://docwilco.github.io/utz/live/index.html). Or use no
  compression to run straight from flash.
- **DST-correct**: lookups return the IANA `tzid`; resolve offsets/DST
  downstream with [`jiff`](https://crates.io/crates/jiff) (whose
  compile-time static zones pair well with μTZ's embedded nature) or the
  prevalent [`chrono-tz`](https://crates.io/crates/chrono-tz).

## Getting started

A quick start needs two choices, picked as cargo features: an
[environment](#environments) (what your target can run: `std`, `alloc`,
or bare-metal `core`) and a [preset](#presets) (a ready-made
size/accuracy point):

```toml
[dependencies]
utz = { version = "0.3", features = ["std", "tiny"] }
```

A preset is a complete build: it bakes its asset into the binary and enables
the decoder features it needs. [`Finder::new()`] loads the one enabled
preset:

```rust
let finder = utz::Finder::new()?;
let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 })?;
assert_eq!(tz, Some("Europe/London"));
```

[`Finder::lookup()`] validates the position (out-of-range or
NaN coordinates are an [`Error`]) and returns an `Option`: `None`
means no zone claims the point (see
[Handling failures](#handling-failures)). To tune any parameter
beyond the presets, see
[Building a custom asset](#building-a-custom-asset).

## Presets

One Cargo feature picks a ready-made size/accuracy point; `custom` instead
generates your own asset with `utz_build`:

| feature       | simplification | geometry    | codec  | size    | notes |
|---------------|----------------|-------------|--------|--------:|-------|
| `tiny`        | ε 10 km, i16   | varint arcs | gzip   |  ~71 KB | needs ~125 KB RAM for the decoded data ([lazy mode](#loading-modes)); brotli/xz would be smaller but need more decode RAM |
| `tiny-static` | ε 10 km, i16   | varint arcs | none   | ~125 KB | `tiny` uncompressed: no decode RAM at all, meant for use straight from flash, runs on bare-metal `core` |
| `compact`     | ε 1 km, i24    | varint arcs | xz     | ~445 KB | |
| `balanced`    | ε 50 m, i24    | varint arcs | brotli | ~1.2 MB | |
| `accurate`    | ε 10 m, i32    | varint arcs | brotli | ~8.1 MB | full zone set (every distinct tzid); the others merge zones identical since now |

Preset features are additive across the whole dependency tree, and
[`Finder::new()`] exists only while exactly one preset is enabled: with
several in the union there is no single default to load, so `new()` is
compiled out and its call sites fail to build. Every enabled preset's asset
stays available as a static in [`data`]; load one explicitly with
[`Finder::from_slice()`] or [`Finder::from_static()`]:

```rust
// with both `tiny` and `tiny-static` enabled:
let lazy = utz::Finder::from_slice(utz::data::TINY)?; // decoded into RAM
let flat = utz::Finder::from_static(utz::data::TINY_STATIC)?; // zero-copy
```

## Configuring

Everything about a build is chosen through cargo features. Three are
mandatory: an asset source (a [preset](#presets) or `custom`), an
[environment](#environments), and at least one [geometry
decoder](#geometry-decoders); only `custom` builds pick the decoder by
hand, presets enable their own. Missing any of the three is a compile
error that lists the options. [Compression
codecs](#compression-codecs) are additive on top. The
[dataset](#datasets) is not a feature: it belongs to the asset, baked
in when the asset is generated. The [`caps`] module exposes at compile
time what a build can read.

### Environments

μTZ is `no_std`-first: API availability follows the environment ladder
`core` ⊂ `alloc` ⊂ `std` (item docs call one step of it a *rung*).
Each level adds API on top of the one below without changing it, which
makes the choice safe to leave to cargo's feature merging: when one
crate in your dependency tree asks for `core` and another for `std`,
the build gets `std` and both keep working.

| feature | environment                              | can load |
|---------|------------------------------------------|----------|
| `core`  | bare metal: no allocator, ~zero heap     | uncompressed assets, zero-copy from flash |
| `alloc` | `no_std` plus an allocator               | compressed assets too, decoded into RAM |
| `std`   | full standard library (implies `alloc`)  | adds file/reader loading |

### Geometry decoders

Each geometry encoding has its own feature; an asset whose encoding
has no compiled decoder is refused at load. Presets enable the decoder
their recipe uses; `custom` users pick the one(s) their assets use.
The measured size/speed ladder is the table on [`GeomEncoding`].

| feature                 | decodes                             | notes |
|-------------------------|-------------------------------------|-------|
| `geom-varint-arcs`      | shared arcs, delta + zigzag varints | the preset encoding; smallest |
| `geom-fixed-width-arcs` | shared arcs, fixed-width coords     | faster streaming reads from flash |
| `geom-full-rings`       | whole rings, read in place          | fastest; little-endian hosts only |
| `geom-coarse`           | grid-only assets                    | cell precision; compiles no point-in-polygon code |

### Compression codecs

Codec features are additive; each compiles the decoder for one payload
codec. Uncompressed assets need none of them. The backend crates are
listed in the [`decompress`] module docs.

| feature    | codec  | minimum environment |
|------------|--------|---------------------|
| `gzip`     | gzip   | `alloc` (pure Rust) |
| `ruzstd`   | zstd   | `alloc` (pure Rust) |
| `zstd-sys` | zstd   | `std` (C libzstd; wins over `ruzstd` when both are enabled) |
| `brotli`   | brotli | `alloc` (pure Rust) |
| `xz`       | xz     | `alloc` (pure Rust) |

### Datasets

The dataset is not a feature: it picks which [timezone-boundary-builder]
release an asset is generated from, baked in at generation time. Its
main knob is the zone set: `now` and `1970` merge zones whose rules are
identical since that date, while `all` keeps every distinct tzid. Every
preset except `accurate` uses `now`, the smallest; custom builds
choose. Zone counts, ocean coverage, and the `land-` variants are
documented with [`utz_build`].

## Loading an asset

Loading is build-once/query-many: a constructor validates the header
and decompresses and decodes exactly once, up front, and the returned
[`Finder`] never repeats any of it. Lookups borrow the `Finder`
immutably and allocate nothing, so construct one at startup (or lazily
behind e.g. `std::sync::OnceLock`), keep it alive, and route every
query through it; constructing per query would repay the whole decode
cost each time.

### Loading modes

An asset this build cannot read (a missing [geometry
decoder](#geometry-decoders) or [codec](#compression-codecs)) is refused
with a typed error before any decoding starts. Decompression allocates
exactly one buffer: the header states the decompressed size up front. RAM
use then follows from how the asset was loaded:

- **zero-copy** ([`Finder::from_static()`]): the asset is borrowed in
  place and lookups stream geometry straight off the stored bytes, with
  no heap allocation at all.
- **lazy** ([`Finder::from_slice()`] and friends): the decompressed payload
  lives in owned RAM and nothing else is cached (the RAM notes in the
  [preset table](#presets) are this buffer).
- **eager** ([`Finder::preload()`]): all rings are additionally decoded up
  front into a flat cache, the fastest mode; [`Finder::preload_bytes()`]
  tells you the exact cost before you pay it.
- **eager from compressed** ([`Finder::eager_from_slice()`]): the asset
  decodes straight to the eager cache and the encoded geometry is
  dropped, for less steady-state RAM than lazy + [`Finder::preload()`]
  combined.

### Pairing assets with constructors

Three choices together set where a build lands on RAM and speed: the
asset's codec, its [geometry encoding](#geometry-decoders), and the
constructor that loads it. The recurring trade is that spending storage
(a larger encoding, no compression) removes lookup work and RAM. Every
row below is a one- or two-knob variant of the `compact` recipe
([`utz_build::Config`] code in the first column); the relative ladder
holds for the other recipes and is
quantified on [`GeomEncoding`].

| asset | loaded with | minimum environment | asset size | steady-state RAM | lookup speed |
|-------|-------------|---------------------|-----------:|-----------------:|--------------|
| <code>Config::compact()<br>&nbsp;&nbsp;.codec(Codec::Uncompressed)</code> | [`Finder::from_static()`] | `core` | ~608 KB | none | baseline |
| <code>Config::compact()<br>&nbsp;&nbsp;.codec(Codec::Uncompressed)<br>&nbsp;&nbsp;.geom(GeomEncoding::FixedWidthArcs)</code> | [`Finder::from_static()`] | `core` | ~1.0 MB | none | near-eager |
| <code>Config::compact()<br>&nbsp;&nbsp;.codec(Codec::Uncompressed)<br>&nbsp;&nbsp;.geom(GeomEncoding::FullRings)</code> | [`Finder::from_static()`] | `core` | ~1.9 MB | none | eager |
| <code>Config::compact()<br>&nbsp;&nbsp;.codec(Codec::Uncompressed)</code> | [`Finder::from_static()`] + [`Finder::preload()`] | `alloc` | ~608 KB | ~2.4 MB | eager |
| <code>Config::compact()</code> | [`Finder::from_slice()`] / [`Finder::from_vec()`] | `alloc` | ~445 KB | ~608 KB | baseline |
| <code>Config::compact()</code> | [`Finder::eager_from_slice()`] | `alloc` | ~445 KB | ~2.5 MB | eager |
| <code>Config::compact()<br>&nbsp;&nbsp;.codec(Codec::Uncompressed)<br>&nbsp;&nbsp;.geom(GeomEncoding::Coarse)</code> | [`Finder::from_static()`] | `core` | ~81 KB | none | grid probe |

All rows share the `compact` recipe's simplification and quantization
(RDP ε 1 km, population-weighted, i24 coordinates); the sizes were
measured on TZBB 2026c.

`FullRings` + [`Finder::from_static()`] is the standout: the encoding is
the preload cache serialized, so lookups run at eager speed straight off
flash with no heap and no preload pass at boot; it just costs the most
storage (and less than the cache it replaces: flash coordinates stay
quant-width while the RAM cache rounds up to i32). Compression pulls
the other way: it yields the smallest asset, but the decoded payload (or
after [`Finder::eager_from_slice()`], the eager cache) must live in
RAM. `Coarse` sidesteps the trade entirely by dropping the polygons:
there is near-nothing to store and, via [`Finder::from_static()`], no
RAM either (the floor of both ladders), at cell precision.

### Handling failures

A lookup distinguishes a bad question from an empty answer:
[`Finder::lookup()`] errors with [`Error::InvalidPosition`]
when the position itself is out of range (lon beyond ±180, lat beyond
±90, or NaN), and returns `Ok(None)` when the position is fine but no
zone claims it, which with oceans covered (the default datasets)
never happens, and on a `land-` dataset means a point at sea. Hot
loops whose inputs are valid by construction can skip the check with
[`Finder::lookup_unchecked()`].

Everything else fallible happens at load, and [`Error`]'s variants
sort into three families:

- **capability mismatches** ([`Error::CodecNotCompiledIn`],
  [`Error::GeometryNotCompiledIn`]): the build lacks a feature the
  asset needs; fix the feature list. For `custom` builds the generated
  guard file turns these into compile errors instead.
- **data problems** ([`Error::Truncated`], [`Error::BadMagic`],
  [`Error::UnsupportedVersion`], and the other header/decode errors):
  the bytes are not a readable μTZ asset. This is the family an OTA
  update path should expect and handle by rejecting the download.
- **usage errors**: [`Error::StaticAssetCompressed`] means decompression
  needs an owned buffer, so use [`Finder::from_slice()`], and
  [`Error::Misaligned`] is fixed by embedding `FullRings` assets with
  [`include_bytes_aligned!`].

## Building a custom asset

The `custom` asset source pairs with the `utz_build` crate. In a
`build.rs` (with `utz_build` as a build-dependency), the typed builder
([`utz_build::Config`]) fetches the source data into a cache, encodes, and
writes the asset plus a guard file:

```rust
// build.rs
utz_build::Config::new()
    .dataset("now")     // [land-]now | 1970 | all
    .rdp_meters(500.0)  // simplification tolerance ceiling
    .quant_bits(24)     // 16 / 24 / 32
    .codec(utz_build::Codec::Gzip)
    .generate()?;       // writes $OUT_DIR/tz.utz (+ .guard.rs)
```

Preset recipes double as starting points for one-knob variants:
`Config::tiny().codec(Codec::Uncompressed)` is exactly the `tiny-static`
recipe.

The features must then match the asset: `custom`, an
[environment](#environments), the [geometry decoder](#geometry-decoders) for
its encoding (`geom-varint-arcs` for the default), and the [codec
feature](#compression-codecs) for its compression (none for
`Codec::Uncompressed`):

```toml
[dependencies]
utz = { version = "0.3", features = ["std", "custom", "gzip", "geom-varint-arcs"] }

[build-dependencies]
utz_build = "0.3"
```

The generated guard file asserts exactly this match. Embed the asset and
`include!` the guard next to it, and a feature mismatch becomes a compile
error instead of a load error:

```rust
include!(concat!(env!("OUT_DIR"), "/tz.utz.guard.rs"));
static TZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tz.utz"));
let finder = utz::Finder::from_slice(TZ)?;
```

Uncompressed assets can instead be borrowed zero-copy with
[`Finder::from_static()`] (full-rings assets must be 4-byte aligned: embed
those with the re-exported [`include_bytes_aligned!`]). Outside a
`build.rs`, the `utz_build_cli` binary writes the same assets: `utz_build_cli gen
now 500 --qbits 24 --codec gzip -o tz.utz`.

What the built asset then costs at runtime is set by which constructor
loads it: see [Loading an asset](#loading-an-asset), especially the
[pairing table](#pairing-assets-with-constructors), whose rows are
`Config` variants of exactly this kind.

## How it works

### Whittling the data down

An asset starts as the [timezone-boundary-builder] `GeoJSON` (~80 MB of
source data for the default dataset) and is reduced in stages when it is
generated:

1. **Zone set**: the [dataset](#datasets) choice alone removes most zones:
   `now` and `1970` merge ones whose rules are identical since that date
   (`all` keeps every tzid).
2. **Topology**: borders shared between adjacent zones are cut into arcs at
   junction points and each arc is stored once; rings become lists of arc
   references. With oceans covered (the default) the zones tile the whole
   planet, every border is shared by exactly two zones, and every coordinate
   appears twice in the source, so this halves them; land-only datasets save
   a little less (coastlines bound one zone).
3. **Simplification**: each arc is simplified once (RDP by default) to the
   configured tolerance, so neighboring zones stay perfectly stitched.
   Optional population-density weighting keeps densely-inhabited borders
   detailed while relaxing empty ones.
4. **Quantization**: coordinates land on an integer grid at 16, 24, or 32
   bits per coordinate; narrower grids mean smaller assets and narrower
   arithmetic at lookup time. Each width spreads lon ±180° / lat ±90°
   across its integer range, so one grid step is:

   | grid | lat step | lon step | on the ground at the equator |
   |------|---------:|---------:|-----------------------------:|
   | i16  | 2.7e-3°  | 5.5e-3°  | ~306 m N–S × ~611 m E–W      |
   | i24  | 1.1e-5°  | 2.1e-5°  | ~1.2 m × ~2.4 m              |
   | i32  | 4.2e-8°  | 8.4e-8°  | ~4.7 mm × ~9.3 mm            |

   Rounding moves a point by at most half a step, and the E–W ground
   step shrinks with cos(lat) away from the equator (at 60° latitude it
   matches the N–S step).
5. **Coordinate coding**: within an arc, vertices are stored as
   zigzag-varint deltas, a byte or two for most steps (the other [geometry
   encodings](#geometry-decoders) trade that compactness for lookup speed).
6. **Grid prefilter**: a coarse lon/lat grid is rasterized so most queries
   never touch geometry at all: an interior cell answers with its zone
   directly, a border cell carries a short candidate-polygon list.
7. **Compression**: the payload is compressed with the chosen codec; a
   small plaintext header stays readable so any tool can identify the
   asset without decompressing it.

The stages were measured per preset (TZBB release 2026c; the workspace's
`utz_dev_cli whittle` command reproduces every row):

<table>
  <thead>
    <tr><th>stage</th><th><code>tiny</code></th><th><code>compact</code></th><th><code>balanced</code></th><th><code>accurate</code></th></tr>
  </thead>
  <tbody>
    <tr><td>source <code>GeoJSON</code></td><td colspan="3" align="center">80.6 MB</td><td align="right">173.9 MB</td></tr>
    <tr><td>parsed coordinates (f64 pairs)</td><td colspan="3" align="center">57.8 MB</td><td align="right">124.9 MB</td></tr>
    <tr><td>shared-arc topology</td><td colspan="3" align="center">28.9 MB</td><td align="right">62.4 MB</td></tr>
    <tr><td>simplified</td><td align="right">692.6 KB</td><td align="right">2.5 MB</td><td align="right">7.9 MB</td><td align="right">30.8 MB</td></tr>
    <tr><td>quantized (coords at full width)</td><td align="right">114.4 KB</td><td align="right">940.6 KB</td><td align="right">3.0 MB</td><td align="right">15.4 MB</td></tr>
    <tr><td>varint-coded arcs</td><td align="right">73.4 KB</td><td align="right">512.6 KB</td><td align="right">1.6 MB</td><td align="right">10.1 MB</td></tr>
    <tr><td>serialized payload (grid added)</td><td align="right">124.9 KB</td><td align="right">607.6 KB</td><td align="right">1.9 MB</td><td align="right">10.6 MB</td></tr>
    <tr><td>compressed asset</td><td align="right">70.9 KB</td><td align="right">445.1 KB</td><td align="right">1.2 MB</td><td align="right">8.1 MB</td></tr>
  </tbody>
</table>

The first three presets share the `now` dataset (64 zones after the
stage-1 merge), so their rows only diverge once simplification applies
each recipe's tolerance; `accurate` starts from the full `all` set
(444 zones). `tiny-static` is `tiny`'s serialized payload shipped
uncompressed: it is the 124.9 KB row with stage 7 skipped.

### Shipping the asset

The result is one self-describing asset (the [`format`][`crate::format`]
module documents the layout): the header records every knob, so the decoder
is fully generic and one binary reads any variant handed to it. The asset
reaches the reader either compiled in (a preset's data crate, or your
`build.rs` output via `include_bytes!`) or as external data: a file, an OTA
download, a dedicated flash partition. Uncompressed assets can be used
where they lie: [`Finder::from_static()`] borrows them zero-copy straight
from memory-mapped flash.

### Lookup

A lookup quantizes the query point and indexes the grid cell; in the common
case that already answers it. On a border cell it walks the candidate
polygons: a bounding-box gate first, then an exact integer even-odd
point-in-polygon test. There is no floating point in the path, so results
are identical on every target, and points exactly on a border are claimed
deterministically. [`Finder::lookup_coarse()`] skips geometry entirely and
answers at cell precision from any asset.

## Inspirations & credits

μTZ stands on the shoulders of three excellent projects; it reuses their
ideas and pushes on size and embeddability:

- **[spatialtime](https://github.com/moranbw/spatialtime)** is the crate
  μTZ grew out of. The build-once/query-many API shape and the
  compression approach come from here.
- **[rtz](https://github.com/twitchax/rtz)** contributed the 1°×1° grid
  prefilter.
- **[tzf-rs](https://github.com/ringsaturn/tzf-rs)** contributed
  shared-edge (topology) boundary deduplication, the grid/preindex
  fast-path (its "Fuzzy" finder, μTZ's `lookup_coarse()`), and
  delta+varint coordinate encoding.

Where those ship one fixed size/accuracy point, μTZ makes the tradeoff a
build-time knob and adds integer quantization to go ~10× smaller, with a
genuinely `no_std`/flash-embeddable format.

The data comes from [timezone-boundary-builder] (derived from
[OpenStreetMap], [ODbL]) and, for population-density-weighted
simplification, the European Commission JRC's
[GHS-POP](https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php)
raster (CC BY 4.0). The simplification menu is classic computational
geometry: Ramer–Douglas–Peucker (1972/1973), Visvalingam–Whyatt (1993),
and Imai–Iri (1988); the [`utz_simplify`] crate documents each with
citations and guarantees.

[`utz_build::Config`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html
[`utz_build`]: https://docwilco.github.io/utz/docs/utz_build/index.html#datasets
[`utz_simplify`]: https://docwilco.github.io/utz/docs/utz_simplify/index.html
[timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
[OpenStreetMap]: https://www.openstreetmap.org/
[ODbL]: https://opendatacommons.org/licenses/odbl/

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**). Preset assets are simplified with
population-density weighting derived from
[GHS-POP R2023A](https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php)
(European Commission JRC, **CC BY 4.0**).

[`Finder`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html
[`Finder::new()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.new
[`Finder::from_vec()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_vec
[`Finder::eager_from_slice()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.eager_from_slice
[`Error`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html
[`Error::BadMagic`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.BadMagic
[`Error::Truncated`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.Truncated
[`Error::UnsupportedVersion`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.UnsupportedVersion
[`Error::CodecNotCompiledIn`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.CodecNotCompiledIn
[`Error::GeometryNotCompiledIn`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.GeometryNotCompiledIn
[`Error::StaticAssetCompressed`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.StaticAssetCompressed
[`Error::Misaligned`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.Misaligned
[`Finder::from_slice()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_slice
[`Finder::from_static()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_static
[`Finder::preload()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload
[`Finder::preload_bytes()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload_bytes
[`Finder::lookup()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup
[`Finder::lookup_unchecked()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_unchecked
[`Error::InvalidPosition`]: https://docwilco.github.io/utz/docs/utz/enum.Error.html#variant.InvalidPosition
[`Finder::lookup_coarse()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_coarse
[`data`]: https://docwilco.github.io/utz/docs/utz/data/index.html
[`caps`]: https://docwilco.github.io/utz/docs/utz/caps/index.html
[`crate::format`]: https://docwilco.github.io/utz/docs/utz/format/index.html
[`decompress`]: https://docwilco.github.io/utz/docs/utz/decompress/index.html
[`GeomEncoding`]: https://docwilco.github.io/utz/docs/utz/enum.GeomEncoding.html
[`include_bytes_aligned!`]: https://docwilco.github.io/utz/docs/utz/macro.include_bytes_aligned.html
