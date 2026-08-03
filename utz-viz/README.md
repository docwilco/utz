# utz-viz

Everything behind the deployed webdist viewer: the whittle stage
ladder `utz-dev-cli whittle` prints, the misassignment pricing, the
wasm surface the live page runs, and the site emitter.

The ladder mirrors the utz crate docs' "How it works" stages: parsed
f64 coordinates → shared-arc topology → simplification → quantized +
serialized sections → compressed asset. The [`misassign`] module is
the accuracy side of the same story: the misassigned-area/population
pricing the viewer's simplify worker runs (through the `wasm`
exports) and the accuracy command shares natively. The [`emit`]
module (default `emit` feature; off for the wasm build) writes the
static site: page, per-dataset blobs, heat raster.

[`misassign`]: https://docwilco.github.io/utz/docs/utz_viz/misassign/index.html
[`emit`]: https://docwilco.github.io/utz/docs/utz_viz/emit/index.html

License: MIT
