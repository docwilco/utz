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

## Getting started

Pick an environment and a preset in `Cargo.toml`:

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
zero-copy).

## Preset bundles

One Cargo feature picks a ready-made size/accuracy point; `custom`
instead generates your own asset with `utz-build`:

| feature       | simplification | size    | notes |
|---------------|----------------|--------:|-------|
| `tiny`        | ε 10 km, i16   |  ~71 KB | gzip: ~125 KB RAM to decode |
| `tiny-static` | ε 10 km, i16   | ~125 KB | `tiny` uncompressed: zero-copy from flash, ~0 RAM, runs on bare-metal `core` |
| `compact`     | ε 1 km, i24    | ~445 KB | xz |
| `balanced`    | ε 50 m, i24    | ~1.3 MB | brotli |
| `accurate`    | ε 10 m, i32    | ~8.3 MB | brotli: full zone set (every distinct tzid); the others merge zones identical since now |

## Configuring with features

Every build makes three choices; forgetting one is a compile error
whose message explains the options.

1. **Data tier**: a preset from the table above, or `custom` (bring
   your own asset, generated with `utz-build`).
2. **Environment**: `std`, `alloc` (`no_std` with an allocator), or
   `core` (bare metal: uncompressed assets only, near-zero heap). The
   ladder is strict (`core` ⊂ `alloc` ⊂ `std`), so feature unions
   across a dependency tree resolve upward.
3. **Geometry decoder**: one `geom-*` feature per encoding
   (`geom-varint-arcs`, `geom-fixed-width-arcs`, `geom-full-rings`,
   `geom-coarse`; the `GeomEncoding` docs carry the size/speed
   ladder). Presets enable the decoder their recipe uses; `custom`
   users pick the one(s) their assets use. A container whose encoding
   has no compiled decoder is refused at load.

Codec features are additive: each of `gzip`, `ruzstd`, `zstd-sys`,
`brotli`, and `xz` compiles the decoder for one payload codec (all
but `zstd-sys` are pure Rust and `no_std`-clean; see the `decompress`
module). Uncompressed assets need none of them. The `caps` module
exposes at compile time what a build can read.

## Building a custom dataset

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
`tiny-static` recipe. Then embed the asset and `include!` the guard
file, which turns a feature mismatch into a compile error instead of
a load error:

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

## How it works

Self-describing container (see the `format` module) → one generic decoder: grid
prefilter, then per-polygon integer PIP. Three memory modes, selected by
how the container is loaded: **zero-copy** (uncompressed asset
borrowed from any static source), **lazy** (payload decompressed into
owned RAM, no decoded-geometry cache), **eager** (`Finder::preload`:
all rings decoded up front). `no_std`-first: API availability follows
the environment ladder `core` ⊂ `alloc` ⊂ `std`.

## Inspirations & credits

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
