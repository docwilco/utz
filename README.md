# uTZ — micro-timezone

Tiny, embeddable latitude/longitude → IANA timezone lookup for Rust.

> **Status: work in progress.** The design is settled (see [PLAN.md](PLAN.md));
> the engine is being implemented.

## Why

- **Tiny** — OSM timezone data down to ~125–460 KB via shared-arc topology,
  tunable RDP simplification, integer quantization, and general compression.
- **Embeddable** — pure-Rust codecs, integer point-in-polygon, flat arrays that
  borrow zero-copy from a flash partition. Targets ESP32/Xtensa-class devices.
- **Tunable** — pick dataset, RDP tolerance, quantization grid, grid cell size,
  and codec to hit your exact size / RAM / accuracy point, guided by a
  visualization tool.
- **DST-correct** — returns the IANA `tzid`; resolve offsets/DST downstream (e.g.
  `chrono-tz`). A `-1970` dataset option gives per-location-correct tzids.

```rust
let finder = utz::Finder::new()?;              // or ::from_static(flash_bytes)
let tz = finder.lookup(-0.1278, 51.5074);      // Some("Europe/London")
```

## Inspirations & credits

uTZ stands on the shoulders of three excellent projects — it reuses their ideas
and pushes on size and embeddability:

- **[spatialtime](https://github.com/moranbw/spatialtime)** — the crate uTZ grew
  out of. The `Reader`-style build-once/query-many API and the OSM data approach
  come from here.
- **[rtz](https://github.com/twitchax/rtz)** — the 1°×1° grid prefilter and the
  decode-once-into-memory lookup model.
- **[tzf-rs](https://github.com/ringsaturn/tzf-rs)** — shared-edge (topology)
  boundary deduplication, the grid/preindex fast-path (its "Fuzzy" finder, uTZ's
  `lookup_coarse`), and delta+varint
  coordinate encoding.

Where those ship fixed data tiers, uTZ makes the size/accuracy tradeoff a
build-time knob and adds general-purpose compression + integer quantization to go
~10× smaller, with a genuinely `no_std`/flash-embeddable format.

## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**) — downloaded and built on demand, never committed.
