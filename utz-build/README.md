# utz-build

μTZ builder library.

Home of the encoder (topology + RDP + quantization + grid + container),
source loading, density weighting, and the viz generator. The
`utz-build-cli` binary (`gen` plus the measurement and bench
subcommands) lives in the crate of the same name, which also carries
the runtime-reader dependency; this library stays reader-free so build
scripts using [`Config`] are not rebuilt by reader-only changes.

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

[timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
[`Config`]: https://docwilco.github.io/utz/docs/utz_build/config/struct.Config.html

License: MIT
