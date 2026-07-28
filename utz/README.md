# μTZ

μTZ (micro-timezone): tiny, tunable, embeddable lat/lon → IANA timezone-id lookup.

- **Tiny**: OSM timezone data down from ~80 MB to ~70 KB via shared-arc
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
enables the decoder features it needs. [`Finder::new()`] loads the one
enabled preset:


## License

Code: MIT. Timezone data is derived from
[timezone-boundary-builder](https://github.com/evansiroky/timezone-boundary-builder)
(OpenStreetMap, **ODbL**)

[`Finder::new`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.new
[`Finder::from_slice`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_slice
[`Finder::from_static`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.from_static
[`Finder::preload`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload
[`Finder::preload_bytes`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.preload_bytes
[`Finder::lookup_coarse`]: https://docwilco.github.io/utz/docs/utz/struct.Finder.html#method.lookup_coarse
[`data`]: https://docwilco.github.io/utz/docs/utz/data/index.html
[`caps`]: https://docwilco.github.io/utz/docs/utz/caps/index.html
[`crate::format`]: https://docwilco.github.io/utz/docs/utz/format/index.html
[`decompress`]: https://docwilco.github.io/utz/docs/utz/decompress/index.html
[`GeomEncoding`]: https://docwilco.github.io/utz/docs/utz/enum.GeomEncoding.html
[`include_bytes_aligned!`]: https://docwilco.github.io/utz/docs/utz/macro.include_bytes_aligned.html
