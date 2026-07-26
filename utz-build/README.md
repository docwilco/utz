# utz-build

μTZ build + exploration crate.

Home of the encoder (topology + RDP + quantization + grid + container)
and the measurement commands. Also hosts the viz tool.

The source is OSM timezone-boundary-builder. Datasets pick the merge
vintage — `now` (65 zones, default), `1970` (304 zones), or `all`
(444 zones) — with oceans by default; a `land-` prefix selects the
land-only releases.

License: MIT
