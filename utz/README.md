# μTZ

μTZ (micro-timezone): tiny, tunable, embeddable lat/lon → IANA timezone-id
lookup.

- **Tiny**: OSM timezone data down from ~60 MB to ~70 KB via shared-arc
  topology, tunable map simplification, integer quantization, and general
  compression. Larger more accurate options available as well.
- **Embeddable**: pure-Rust codecs, integer point-in-polygon, `no_std`
  capable.
- **Tunable**: pick dataset, simplification parameters, data types,
  quantization grid, grid cell size, and compression codec to hit your exact
  size / RAM / accuracy point, guided by a [visualization
  tool](https://docwilco.github.io/utz/live/index.html). Or use no
  compression for direct from flash.
- **DST-correct**: returns the IANA `tzid`; resolve offsets/DST downstream
  with [`jiff`](https://crates.io/crates/jiff) (whose compile-time static
  zones pair well with μTZ's embedded nature) or the prevalent `chrono-tz`.

# Getting started

A quick start needs just two choices, picked as cargo features: an
[environment](#environments) and a [preset](#preset-bundles).

```toml
[dependencies]
utz = { version = "0.1", features = ["std", "tiny"] }
```

A preset is a complete build: it bakes its asset into the binary and enables
the decoder features it needs. [`Finder::new()`] loads the one enabled
preset:

```rust
let finder = utz::Finder::new()?;
let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 });
assert_eq!(tz, Some("Europe/London"));
```

With more than one preset feature selected, pick explicitly via the statics
in the [`data`] module: `Finder::from_slice(utz::data::TINY)` (compressed)
or `Finder::from_static(utz::data::TINY_STATIC)` (uncompressed, zero-copy).
If you want to tune any of the parameters (simplification, quantization,
codec, dataset), see [Building a custom asset](#building-a-custom-asset).

# Preset bundles

One Cargo feature picks a ready-made size/accuracy point; `custom` instead
generates your own asset with `utz-build`:

| feature       | simplification | geometry    | codec  | size    | notes |
|---------------|----------------|-------------|--------|--------:|-------|
| `tiny`        | ε 10 km, i16   | varint arcs | gzip   |  ~71 KB | Needs ~125 KB RAM to hold the uncompressed data, brotli/xz would be smaller, but need more RAM for the decompression process |
| `tiny-static` | ε 10 km, i16   | varint arcs | none   | ~125 KB | `tiny` uncompressed: doesn't need additional RAM to hold the data, meant for use straight from flash, runs on bare-metal `core` |
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

# Configuring

Everything about a build is chosen through cargo features. Three are
mandatory: a data tier (a [preset](#preset-bundles) or `custom`), an
[environment](#environments), and at least one [geometry
decoder](#geometry-decoders); only `custom` builds pick the decoder by
hand, presets enable their own. Missing any of the three is a compile
error that lists the options. [Compression
codecs](#compression-codecs) are additive on top. The
[dataset](#datasets) is not a feature: it belongs to the asset, baked
in when the asset is generated. The [`caps`] module exposes at compile
time what a build can read.

## Environments

μTZ is `no_std`-first: API availability follows the environment ladder
`core` ⊂ `alloc` ⊂ `std`. Each level adds API on top of the one below
without changing it, which makes the choice safe to leave to cargo's
feature merging: when one crate in your dependency tree asks for `core`
and another for `std`, the build gets `std` and both keep working.

| feature | environment                              | can load |
|---------|------------------------------------------|----------|
| `core`  | bare metal: no allocator, ~zero heap     | uncompressed assets, zero-copy from flash |
| `alloc` | `no_std` plus an allocator               | compressed assets too, decoded into RAM |
| `std`   | full standard library (implies `alloc`)  | adds file/reader loading |

## Geometry decoders

One feature per geometry encoding; an asset whose encoding has no
compiled decoder is refused at load. Presets enable the decoder their recipe
uses; `custom` users pick the one(s) their assets use. The measured
size/speed ladder is the table on [`GeomEncoding`].

| feature                 | decodes                             | notes |
|-------------------------|-------------------------------------|-------|
| `geom-varint-arcs`      | shared arcs, delta + zigzag varints | the preset encoding; smallest |
| `geom-fixed-width-arcs` | shared arcs, fixed-width coords     | faster streaming reads from flash |
| `geom-full-rings`       | whole rings, read in place          | fastest; little-endian hosts only |
| `geom-coarse`           | grid-only assets                    | cell precision; compiles no point-in-polygon code |

## Compression codecs

Additive; each compiles the decoder for one payload codec. Uncompressed
assets need none of them. The backend crates are listed in the
[`decompress`] module docs.

| feature    | codec  | minimum environment |
|------------|--------|---------------------|
| `gzip`     | gzip   | `alloc` (pure Rust) |
| `ruzstd`   | zstd   | `alloc` (pure Rust) |
| `zstd-sys` | zstd   | `std` (C libzstd; wins over `ruzstd` when both are enabled) |
| `brotli`   | brotli | `alloc` (pure Rust) |
| `xz`       | xz     | `alloc` (pure Rust) |

# Datasets

Not a feature: the dataset picks which timezone-boundary-builder release an
asset is generated from, baked in at generation time. Its main knob is the
zone set: `now` and `1970` merge zones whose rules are identical since that
date, while `all` keeps every distinct tzid. Every preset except `accurate`
uses `now`, the smallest; custom builds choose. Zone counts, ocean coverage,
and the `land-` variants are documented with [`utz_build`].

# Building a custom asset

The `custom` tier pairs with the `utz-build` crate. In a `build.rs` (with
`utz-build` as a build-dependency), the typed builder
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
utz = { version = "0.1", features = ["std", "custom", "gzip", "geom-varint-arcs"] }

[build-dependencies]
utz-build = "0.1"
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
`build.rs`, the `utz-build-cli` binary writes the same assets: `utz-build-cli gen
now 500 --qbits 24 --codec gzip -o tz.utz`.

What the built asset then costs at runtime is set by which constructor
loads it: [Decoding and lookup](#decoding-and-lookup) explains the
modes, and [Pairing assets with
constructors](#pairing-assets-with-constructors) tabulates the
worthwhile knob combinations, each row a `Config` one-liner.

# How it works

## Whittling the data down

An asset starts as the timezone-boundary-builder `GeoJSON` (~80 MB of source
data for the default dataset) and is reduced in stages when it is generated
(the `utz-build-cli whittle` command measures every stage per preset):

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
   arithmetic at lookup time.
5. **Coordinate coding**: within an arc, vertices are stored as
   zigzag-varint deltas, a byte or two for most steps (the other [geometry
   encodings](#geometry-decoders) trade that compactness for lookup speed).
6. **Grid prefilter**: a coarse lon/lat grid is rasterized so most queries
   never touch geometry at all: an interior cell answers with its zone
   directly, a border cell carries a short candidate-polygon list.
7. **Compression**: the section blob is compressed with the chosen codec;
   only the format prologue and header stay plaintext.

## Shipping the asset

The result is one self-describing asset (the [`format`][`crate::format`]
module documents it): the header records every knob, so the decoder is fully
generic and one binary reads any variant handed to it. The asset reaches
the reader either compiled in (a preset's data crate, or your `build.rs`
output via `include_bytes!`) or as external data: a file, an OTA download, a
dedicated flash partition. Uncompressed assets can be used where they
lie: [`Finder::from_static()`] borrows them zero-copy straight from
memory-mapped flash.

## Decoding and lookup

Loading is build-once/query-many: a constructor validates the header
and decompresses and decodes exactly once, up front, and the returned
[`Finder`] never repeats any of it. Lookups borrow the `Finder`
immutably and allocate nothing, so construct one at startup (or lazily
behind e.g. `std::sync::OnceLock`), keep it alive, and route every
query through it; constructing per query would repay the whole decode
cost each time.

An asset this build cannot read (a missing [geometry
decoder](#geometry-decoders) or [codec](#compression-codecs)) is refused
with a typed error before any decoding starts. Decompression allocates
exactly one buffer: the header states the decompressed size up front. RAM
use then follows from how the asset was loaded:

- **zero-copy** ([`Finder::from_static()`]): the asset is borrowed in
  place and lookups stream geometry straight off the stored bytes; no heap
  allocation at all.
- **lazy** ([`Finder::from_slice()`] and friends): the decompressed payload
  lives in owned RAM and nothing else is cached (the RAM notes in the
  [preset table](#preset-bundles) are this buffer).
- **eager** ([`Finder::preload()`]): all rings are additionally decoded up
  front into a flat cache, the fastest mode; [`Finder::preload_bytes()`]
  tells you the exact cost before you pay it.

A lookup quantizes the query point and indexes the grid cell; in the common
case that already answers it. On a border cell it walks the candidate
polygons: a bounding-box gate first, then an exact integer even-odd
point-in-polygon test. There is no floating point in the path, so results
are identical on every target, and points exactly on a border are claimed
deterministically. [`Finder::lookup_coarse()`] skips geometry entirely and
answers at cell precision from any asset.

## Pairing assets with constructors

Three choices together set where a build lands on RAM and speed: the
asset's codec, its [geometry encoding](#geometry-decoders), and the
constructor that loads it. The recurring trade: spending storage (a
larger encoding, no compression) removes lookup work and RAM. Every
row below is a one- or two-knob
variant of the `compact` recipe ([`utz_build::Config`] code in the
first column); the relative ladder holds for the other recipes and is
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
(RDP ε 1 km, population-weighted, i24 coordinates); sizes measured on
TZBB 2026c.

`FullRings` + [`Finder::from_static()`] is the standout: the encoding is
the preload cache serialized, so lookups run at eager speed straight off
flash with no heap and no preload pass at boot; it just costs the most
storage (and less than the cache it replaces: flash coordinates stay
quant-width while the RAM cache rounds up to i32). Compression pulls
the other way: the smallest asset, but the decoded payload (or
after [`Finder::eager_from_slice()`], the eager cache) must live in
RAM. `Coarse` sidesteps the trade entirely by dropping the polygons:
near-nothing to store, and via [`Finder::from_static()`] no RAM either
(the floor of both ladders), at cell precision.

# Inspirations & credits

μTZ stands on the shoulders of three excellent projects; it reuses their
ideas and pushes on size and embeddability:

- **[spatialtime](https://github.com/moranbw/spatialtime)**: the crate μTZ
  grew out of. The `Reader`-style build-once/query-many API and the
  compression approach come from here.
- **[rtz](https://github.com/twitchax/rtz)**: the 1°×1° grid prefilter.
- **[tzf-rs](https://github.com/ringsaturn/tzf-rs)**: shared-edge (topology)
  boundary deduplication, the grid/preindex fast-path (its "Fuzzy" finder,
  μTZ's `lookup_coarse`), and delta+varint coordinate encoding.

Where those ship fixed data tiers, μTZ makes the size/accuracy tradeoff a
build-time knob and adds integer quantization to go ~10× smaller, with a
genuinely `no_std`/flash-embeddable format.

[`utz_build::Config`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html
[`utz_build`]: https://docwilco.github.io/utz/docs/utz_build/index.html#datasets

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**). Preset assets are simplified with
population-density weighting derived from
[GHS-POP R2023A](https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php)
(European Commission JRC, **CC BY 4.0**).

[`Finder::new()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.new
[`Finder::from_slice()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_slice
[`Finder::from_static()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_static
[`Finder::preload()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload
[`Finder::preload_bytes()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload_bytes
[`Finder::lookup_coarse()`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_coarse
[`data`]: https://docwilco.github.io/utz/docs/utz/data/index.html
[`caps`]: https://docwilco.github.io/utz/docs/utz/caps/index.html
[`crate::format`]: https://docwilco.github.io/utz/docs/utz/format/index.html
[`decompress`]: https://docwilco.github.io/utz/docs/utz/decompress/index.html
[`GeomEncoding`]: https://docwilco.github.io/utz/docs/utz/enum.GeomEncoding.html
[`include_bytes_aligned!`]: https://docwilco.github.io/utz/docs/utz/macro.include_bytes_aligned.html
