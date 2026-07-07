# μTZ — micro-timezone lookup
Tiny, embeddable latitude/longitude → IANA timezone lookup for Rust.

> **Status: work in progress.** The design is settled (see [PLAN.md](PLAN.md));
> the engine is being implemented.

## Why

- **Tiny** — OSM timezone data down from 60 MB to ~70 KB via shared-arc topology,
  tunable map simplification, integer quantization, and general compression. Larger
  more accurate options available as well.
- **Embeddable** — pure-Rust codecs, integer point-in-polygon, flat arrays that
  borrow zero-copy from a flash partition. `no_std` capable.
- **Tunable** — pick dataset, simplification parameters, data types,
  quantization grid, grid cell size, and compression codec to hit your exact
  size / RAM / accuracy point, guided by a visualization tool. Or use no
  compression for direct from flash.
- **DST-correct** — returns the IANA `tzid`; resolve offsets/DST downstream with
  [`jiff`](https://crates.io/crates/jiff) (whose compile-time static zones pair
  well with μTZ's embedded story) or the prevalent `chrono-tz`.

```rust
let finder = utz::Finder::new()?;              // or ::from_static(flash_bytes)
let tz = finder.lookup(-0.1278, 51.5074);      // Some("Europe/London")
```

## Preset bundles

One Cargo feature picks a ready-made size/accuracy point (recipes in
PLAN.md §14.5); `custom` instead generates your own asset with `utz-build`:

| feature | simplification | size | notes |
|---|---|---|---|
| `tiny` | ε 10 km, i16 | ~67 KB | gzip — ~119 KB RAM to decode |
| `tiny-static` | ε 10 km, i16 | ~119 KB | `tiny` uncompressed: zero-copy from flash, ~0 RAM, runs on bare-metal `core` |
| `compact` | ε 1 km, i24 | ~441 KB | xz |
| `balanced` | ε 50 m, i24 | ~1.3 MB | brotli |
| `accurate` | ε 10 m, i32 | ~3.9 MB | brotli |

## Inspirations & credits

μTZ stands on the shoulders of three excellent projects — it reuses their ideas
and pushes on size and embeddability:

- **[spatialtime](https://github.com/moranbw/spatialtime)** — the crate μTZ grew
  out of. The `Reader`-style build-once/query-many API and the compression
  approach come from here
- **[rtz](https://github.com/twitchax/rtz)** — the 1°×1° grid prefilter.
- **[tzf-rs](https://github.com/ringsaturn/tzf-rs)** — shared-edge (topology)
  boundary deduplication, the grid/preindex fast-path (its "Fuzzy" finder, μTZ's
  `lookup_coarse`), and delta+varint coordinate encoding.

Where those ship fixed data tiers, μTZ makes the size/accuracy tradeoff a
build-time knob and adds general-purpose compression + integer quantization to go
~10× smaller, with a genuinely `no_std`/flash-embeddable format.

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**)
