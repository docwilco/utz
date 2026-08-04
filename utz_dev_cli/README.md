# utz_dev_cli

The μTZ development toolbox, shipped as the `utz_dev_cli` binary.

This crate holds the repo-internal measurement, bench, and viewer
commands: the tools μTZ is developed with, never published. The
user-facing asset generation
(`gen`, `gen-preset`) lives in the `utz_build_cli` crate; the library
consumers build against is `utz_build`.

```
cargo run --release -p utz_dev_cli -- <subcommand> [args]
```

`visualize` ([`cmd::visualize`]) regenerates the deployed webdist
viewer (page + wasm + per-dataset blobs, via the `utz_viz` crate).

Everything else measures or validates a design decision; each module's
docs state the question it answers and how. The commands group by area:

- The **simplification accuracy** commands are [`cmd::accuracy`],
  [`cmd::density_compare`], [`cmd::rdp_sweep`], and [`cmd::quant_clean`].
- The **asset size** commands are [`cmd::size_table`], [`cmd::whittle`],
  [`cmd::window_sweep`], [`cmd::quant_size`], [`cmd::fixedwidth_size`],
  and [`cmd::imagepack_size`].
- The **grid prefilter design** commands are [`cmd::csr_sweep`],
  [`cmd::gridsweep`], [`cmd::grid2mem`], [`cmd::grid_bench`],
  [`cmd::dominant_cost`], and [`cmd::polygrid_probe`].
- The **validation and sanity** commands are [`cmd::roundtrip`],
  [`cmd::pip_bench`], [`cmd::geoquant`], [`cmd::amscan`], and
  [`cmd::density_probe`].

Source data downloads once into the workspace `cache/` (conditional
GETs keep it fresh); density-weighted runs additionally fetch GHS-POP
(~460 MB) on first use.

[`cmd::accuracy`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/accuracy/index.html
[`cmd::amscan`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/amscan/index.html
[`cmd::csr_sweep`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/csr_sweep/index.html
[`cmd::density_compare`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/density_compare/index.html
[`cmd::density_probe`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/density_probe/index.html
[`cmd::dominant_cost`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/dominant_cost/index.html
[`cmd::fixedwidth_size`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/fixedwidth_size/index.html
[`cmd::geoquant`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/geoquant/index.html
[`cmd::grid2mem`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/grid2mem/index.html
[`cmd::grid_bench`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/grid_bench/index.html
[`cmd::gridsweep`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/gridsweep/index.html
[`cmd::imagepack_size`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/imagepack_size/index.html
[`cmd::pip_bench`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/pip_bench/index.html
[`cmd::polygrid_probe`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/polygrid_probe/index.html
[`cmd::quant_clean`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/quant_clean/index.html
[`cmd::quant_size`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/quant_size/index.html
[`cmd::rdp_sweep`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/rdp_sweep/index.html
[`cmd::roundtrip`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/roundtrip/index.html
[`cmd::size_table`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/size_table/index.html
[`cmd::visualize`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/visualize/index.html
[`cmd::whittle`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/whittle/index.html
[`cmd::window_sweep`]: https://docwilco.github.io/utz/docs/utz_dev_cli/cmd/window_sweep/index.html

License: MIT
