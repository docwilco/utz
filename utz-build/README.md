# utz-build

μTZ builder library.

Home of the encoder (topology + RDP + quantization + grid + container),
source loading, density weighting, and the viz generator. The `utz-build`
binary (`gen` plus the measurement and bench subcommands) lives in the
`utz-build-cli` crate, which also carries the runtime-reader dependency;
this library stays reader-free so build scripts using
[`Config`] are not rebuilt by reader-only changes.

# Datasets

The source data is OSM [timezone-boundary-builder]. The dataset
picks which TZBB release an asset is generated from and is baked
in at generation time. It sets the merge vintage: zones whose
rules are identical from that point on are merged, so older
vintages keep more zones. Oceans are covered by default; a `land-`
prefix selects the land-only releases.

| dataset | zones | merge |
|---------|------:|-------|
| `now`   |    64 | zones identical from today onward merged |
| `1970`  |   304 | zones identical since 1970 merged |
| `all`   |   444 | every distinct tzid kept |

[timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder

License: MIT
