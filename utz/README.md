# μTZ

μTZ (micro-timezone): tiny, tunable, embeddable lat/lon → IANA timezone-id lookup.

- **Tiny**: OSM timezone data down from 60 MB to ~70 KB via shared-arc
  topology, tunable map simplification, integer quantization, and general
  compression. Larger more accurate options available as well.
- **Embeddable**: pure-Rust codecs, integer point-in-polygon, flat
  arrays that borrow zero-copy from a flash partition. `no_std` capable.
- **Tunable**: pick dataset, simplification parameters, data types,
  quantization grid, grid cell size, and compression codec to hit your
  exact size / RAM / accuracy point, guided by a
  [visualization tool](https://docwilco.github.io/utz/live/index.html).
  Or use no compression for direct from flash.
- **DST-correct**: returns the IANA `tzid`; resolve offsets/DST
  downstream with [`jiff`](https://crates.io/crates/jiff) (whose
  compile-time static zones pair well with μTZ's embedded nature) or the
  prevalent `chrono-tz`.

# Getting started

A quick start needs just two choices, picked as cargo features: an
[environment](#environments) and a [preset](#preset-bundles).

```toml
[dependencies]
utz = { version = "0.1", features = ["std", "tiny"] }
```

A preset is a complete build: it bakes its asset into the binary and
enables the decoder features it needs. `Finder::new()` loads the one
enabled preset:

```rust
let finder = utz::Finder::new()?;              // or ::from_static(flash_bytes)
let tz = finder.lookup(utz::Position { lon: -0.1278, lat: 51.5074 });
// Some("Europe/London")
```

With several presets in the tree, pick explicitly via the statics in
the `data` module: `Finder::from_slice(utz::data::TINY)` (compressed)
or `Finder::from_static(utz::data::TINY_STATIC)` (uncompressed,
zero-copy). Everything beyond the presets (your own simplification /
quantization / codec / dataset point) goes through
[Building a custom dataset](#building-a-custom-dataset).

# Preset bundles

One Cargo feature picks a ready-made size/accuracy point; `custom`
instead generates your own asset with `utz-build`:

| feature       | simplification | size    | notes |
|---------------|----------------|--------:|-------|
| `tiny`        | ε 10 km, i16   |  ~71 KB | gzip: ~125 KB RAM to decode |
| `tiny-static` | ε 10 km, i16   | ~125 KB | `tiny` uncompressed: zero-copy from flash, ~0 RAM, runs on bare-metal `core` |
| `compact`     | ε 1 km, i24    | ~445 KB | xz |
| `balanced`    | ε 50 m, i24    | ~1.3 MB | brotli |
| `accurate`    | ε 10 m, i32    | ~8.3 MB | brotli: full zone set (every distinct tzid); the others merge zones identical since now |

# Configuring

A build configures itself entirely through cargo features. Three
choices are mandatory, and forgetting one is a compile error whose
message explains the options: a data tier (a
[preset](#preset-bundles) or `custom`), an
[environment](#environments), and at least one
[geometry decoder](#geometry-decoders) (presets enable their own).
[Compression codecs](#compression-codecs) are additive on top. The
[dataset](#datasets) is a property of the asset rather than of the
build. The `caps` module exposes at compile time what a build can
read.

## Environments

Strict supersets (`core` ⊂ `alloc` ⊂ `std`), so feature unions
across a dependency tree resolve upward:

| feature | environment                              | can load |
|---------|------------------------------------------|----------|
| `core`  | bare metal: no allocator, ~zero heap     | uncompressed assets, zero-copy from flash |
| `alloc` | `no_std` plus an allocator               | compressed assets too, decoded into RAM |
| `std`   | full standard library (implies `alloc`)  | adds file/reader loading |

## Geometry decoders

One feature per geometry encoding; a container whose encoding has no
compiled decoder is refused at load. Presets enable the decoder
their recipe uses; `custom` users pick the one(s) their assets use.
The measured size/speed ladder is the table on `GeomEncoding`.

| feature                 | decodes                             | notes |
|-------------------------|-------------------------------------|-------|
| `geom-varint-arcs`      | shared arcs, delta + zigzag varints | the preset encoding; smallest |
| `geom-fixed-width-arcs` | shared arcs, fixed-width coords     | faster streaming reads from flash |
| `geom-full-rings`       | whole rings, read in place          | fastest; little-endian hosts only |
| `geom-coarse`           | grid-only assets                    | cell precision; compiles no point-in-polygon code |

## Compression codecs

Additive; each compiles the decoder for one payload codec.
Uncompressed assets need none of them. Backend crates and codec
bytes are in the `decompress` module docs.

| feature    | codec  | environment |
|------------|--------|-------------|
| `gzip`     | gzip   | `alloc` (pure Rust) |
| `ruzstd`   | zstd   | `alloc` (pure Rust) |
| `zstd-sys` | zstd   | `std` (C libzstd; wins over `ruzstd` when both are enabled) |
| `brotli`   | brotli | `alloc` (pure Rust) |
| `xz`       | xz     | `alloc` (pure Rust) |

## Datasets

The dataset is baked into an asset when it is generated (every
preset except `accurate` uses `now`; custom builds choose). It picks
the merge vintage: zones whose rules are identical from that point
on are merged, so older vintages keep more zones. Oceans are covered
by default; a `land-` prefix selects the land-only releases.

| dataset | zones | merge |
|---------|------:|-------|
| `now`   |    65 | zones identical from today onward merged |
| `1970`  |   304 | zones identical since 1970 merged |
| `all`   |   444 | every distinct tzid kept |

# Building a custom dataset

The `custom` tier pairs with the `utz-build` crate. In a `build.rs`
(with `utz-build` as a build-dependency), the typed builder fetches
the source data into a cache, encodes, and writes the asset plus a
guard file:

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
`Config::tiny().codec(Codec::Uncompressed)` is exactly the
`tiny-static` recipe.

The features must then match the asset: `custom`, an
[environment](#environments), the [geometry decoder](#geometry-decoders)
for its encoding (`geom-varint-arcs` for the default), and the
[codec feature](#compression-codecs) for its compression (none for
`Codec::Uncompressed`):

```toml
[dependencies]
utz = { version = "0.1", features = ["std", "custom", "gzip", "geom-varint-arcs"] }

[build-dependencies]
utz-build = "0.1"
```

The generated guard file asserts exactly this match. Embed the asset
and `include!` the guard next to it, and a feature mismatch becomes
a compile error instead of a load error:

```rust
include!(concat!(env!("OUT_DIR"), "/tz.utz.guard.rs"));
static TZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tz.utz"));
let finder = utz::Finder::from_slice(TZ)?;
```

Uncompressed assets can instead be borrowed zero-copy with
`Finder::from_static` (full-rings assets must be 4-byte aligned:
embed those with the re-exported `include_bytes_aligned!`). Outside a
`build.rs`, the `utz-build` CLI writes the same containers:
`utz-build gen now 500 --qbits 24 --codec gzip -o tz.utz`.

# How it works

Self-describing container (see the `format` module) → one generic decoder: grid
prefilter, then per-polygon integer PIP. Three memory modes, selected by
how the container is loaded: **zero-copy** (uncompressed asset
borrowed from any static source), **lazy** (payload decompressed into
owned RAM, no decoded-geometry cache), **eager** (`Finder::preload`:
all rings decoded up front). `no_std`-first: API availability follows
the environment ladder `core` ⊂ `alloc` ⊂ `std`.

# Inspirations & credits

μTZ stands on the shoulders of three excellent projects; it reuses
their ideas and pushes on size and embeddability:

- **[spatialtime](https://github.com/moranbw/spatialtime)**: the crate
  μTZ grew out of. The `Reader`-style build-once/query-many API and the
  compression approach come from here.
- **[rtz](https://github.com/twitchax/rtz)**: the 1°×1° grid prefilter.
- **[tzf-rs](https://github.com/ringsaturn/tzf-rs)**: shared-edge
  (topology) boundary deduplication, the grid/preindex fast-path (its
  "Fuzzy" finder, μTZ's `lookup_coarse`), and delta+varint coordinate
  encoding.

Where those ship fixed data tiers, μTZ makes the size/accuracy tradeoff
a build-time knob and adds integer quantization to go ~10× smaller,
with a genuinely `no_std`/flash-embeddable format.

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**)
