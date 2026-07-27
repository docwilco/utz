# utz-build

μTZ builder library.

Home of the encoder (topology + RDP + quantization + grid + container),
source loading, density weighting, and the viz generator. The `utz-build`
binary (`gen` plus the measurement and bench subcommands) lives in the
`utz-build-cli` crate, which also carries the runtime-reader dependency;
this library stays reader-free so build scripts using
[`Config`] are not rebuilt by reader-only changes.

The source is OSM timezone-boundary-builder. Datasets pick the merge
vintage: `now` (65 zones, default), `1970` (304 zones), or `all`
(444 zones). Oceans are included by default; a `land-` prefix selects
the land-only releases.

License: MIT
