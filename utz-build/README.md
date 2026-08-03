# utz-build

μTZ builder library: generate custom `.utz` assets from a `build.rs`.

Everything routes through the typed builder, [`Config`]. In a
`build.rs` (with `utz-build` as a build-dependency):

```rust
utz_build::Config::new()
    .dataset("now")     // [land-]now | 1970 | all
    .rdp_meters(500.0)  // simplification tolerance ceiling
    .quant_bits(24)     // 16 / 24 / 32
    .codec(utz_build::Codec::Gzip)
    .generate()?;       // writes $OUT_DIR/tz.utz (+ .guard.rs)
```

The [utz crate's "Building a custom asset"][custom] section is the
full walkthrough: embedding the asset, the guard file, and matching
reader features. Every knob is a [`Config`] method; everything else
this crate exposes (source loading, density weighting) is machinery
those methods drive.

## Source data

Sources download on first use into a per-user cache directory
(`$XDG_CACHE_HOME/utz-build`, overridable with `UTZ_CACHE_DIR` or
[`Config::cache_dir()`]; see
[`cache_dir()`]) and are revalidated with conditional GETs: the
[timezone-boundary-builder] `GeoJSON` (tens of MB per dataset,
[ODbL]), and for density-weighted recipes the [GHS-POP] population
raster (~460 MB once, [CC BY 4.0]). Set `UTZ_TZBB_RELEASE` to pin a
TZBB release for reproducible builds (see [`loader`]).

[custom]: https://docwilco.github.io/utz/docs/utz/index.html#building-a-custom-asset
[ODbL]: https://opendatacommons.org/licenses/odbl/
[GHS-POP]: https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php
[CC BY 4.0]: https://creativecommons.org/licenses/by/4.0/

## Datasets

The source data is OSM [timezone-boundary-builder]. The dataset
picks which TZBB release an asset is generated from and is baked
in at generation time. Its two knobs are the zone set and ocean
coverage: `now` and `1970` merge zones whose rules are identical
since that date, `all` keeps every distinct tzid, and a `land-`
prefix selects the land-only releases (oceans are covered by
default).

| zone set | zones | `land-` zones | merge |
|----------|------:|--------------:|-------|
| `now`    |    64 |            63 | zones identical from today onward merged |
| `1970`   |   304 |           301 | zones identical since 1970 merged |
| `all`    |   444 |           419 | every distinct tzid kept |

Dropping the oceans does not shrink an asset: the shared-arc
geometry encodings make ocean coverage nearly free, and the
`land-` variants actually measure slightly bigger (at the tiny
recipe's knobs, uncompressed: `land-now` 89.7 KiB vs `now`
86.7 KiB). Their best use is coarse (grid-only) assets that will
only ever be queried for land coordinates: a coarse asset answers
at cell precision, and with oceans covered a land point near the
coast resolves to the ocean timezone whenever most of its cell is
ocean; without ocean zones the cell keeps the land answer.

## Related crates

The [`utz-build-cli`][cli] binary (`gen` and `gen-preset`, the CLI
counterpart of a `build.rs`) lives in its own crate: it carries the
runtime-reader dependency, so this library stays reader-free and
build scripts are not rebuilt by reader-only changes. The generated
assets are read by [`utz`][reader]; the repo-internal measurement
and viewer tooling lives in the unpublished `utz-dev-cli` and
`utz-viz` crates.

[timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
[`Config`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html
[`Config::cache_dir()`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html#method.cache_dir
[`cache_dir()`]: https://docwilco.github.io/utz/docs/utz_build/fn.cache_dir.html
[cli]: https://docwilco.github.io/utz/docs/utz_build_cli/index.html
[`loader`]: https://docwilco.github.io/utz/docs/utz_build/loader/index.html
[reader]: https://docwilco.github.io/utz/docs/utz/index.html

License: MIT
