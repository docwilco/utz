# utz_viz

This crate holds everything behind the deployed webdist viewer: the
misassignment pricing, the wasm surface the live page runs, the site
emitter, and the coordinate/vertex counters that `utz_dev_cli
whittle` prices its stage ladder with.

The [`misassign`] module is the accuracy side of the pipeline story:
the misassigned-area/population pricing the viewer's simplify worker
runs (through the `wasm` exports) and the accuracy command shares
natively. The [`emit`] module (default `emit` feature; off for the
wasm build) writes the static site: the page, the per-dataset blobs,
and the heat raster.

[`misassign`]: https://docwilco.github.io/utz/docs/utz_viz/misassign/index.html
[`emit`]: https://docwilco.github.io/utz/docs/utz_viz/emit/index.html

License: MIT
