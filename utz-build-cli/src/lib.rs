//! μTZ build & measurement CLI: the `utz-build-cli` binary.
//!
//! Not to be confused with the `utz-build` *library*: that reader-free
//! crate is what build scripts depend on. The split keeps build scripts
//! light: this CLI pulls in the runtime reader (`utz`) and measurement
//! dependencies that a `build.rs` never needs.
//!
//! ```text
//! cargo run --release -p utz-build-cli -- <subcommand> [args]
//! ```
//!
//! Two subcommands produce artifacts:
//!
//! - `gen` ([`cmd::encode`]) writes a `.utz` asset: the custom-tier CLI
//!   path, and the input for `utz-bench-cli` and the bench firmware;
//! - `visualize` ([`cmd::visualize`]) regenerates the webdist viewer.
//!
//! Everything else measures or validates a design decision; each module's
//! docs state the question it answers and how. By area:
//!
//! - **simplification accuracy**: [`cmd::accuracy`],
//!   [`cmd::density_compare`], [`cmd::rdp_sweep`], [`cmd::quant_clean`]
//! - **asset size**: [`cmd::size_table`], [`cmd::whittle`],
//!   [`cmd::window_sweep`], [`cmd::quant_size`], [`cmd::fixedwidth_size`],
//!   [`cmd::imagepack_size`]
//! - **grid prefilter design**: [`cmd::csr_sweep`], [`cmd::gridsweep`],
//!   [`cmd::grid2mem`], [`cmd::grid_bench`], [`cmd::dominant_cost`],
//!   [`cmd::polygrid_probe`]
//! - **validation and sanity**: [`cmd::roundtrip`], [`cmd::pip_bench`],
//!   [`cmd::geoquant`], [`cmd::amscan`], [`cmd::density_probe`]
//!
//! Source data downloads once into the workspace `cache/` (conditional
//! GETs keep it fresh); density-weighted runs additionally fetch GHS-POP
//! (~460 MB) on first use.
//!
//! [`cmd::accuracy`]: ../utz_build_cli/cmd/accuracy/index.html
//! [`cmd::amscan`]: ../utz_build_cli/cmd/amscan/index.html
//! [`cmd::csr_sweep`]: ../utz_build_cli/cmd/csr_sweep/index.html
//! [`cmd::density_compare`]: ../utz_build_cli/cmd/density_compare/index.html
//! [`cmd::density_probe`]: ../utz_build_cli/cmd/density_probe/index.html
//! [`cmd::dominant_cost`]: ../utz_build_cli/cmd/dominant_cost/index.html
//! [`cmd::encode`]: ../utz_build_cli/cmd/encode/index.html
//! [`cmd::fixedwidth_size`]: ../utz_build_cli/cmd/fixedwidth_size/index.html
//! [`cmd::geoquant`]: ../utz_build_cli/cmd/geoquant/index.html
//! [`cmd::grid2mem`]: ../utz_build_cli/cmd/grid2mem/index.html
//! [`cmd::grid_bench`]: ../utz_build_cli/cmd/grid_bench/index.html
//! [`cmd::gridsweep`]: ../utz_build_cli/cmd/gridsweep/index.html
//! [`cmd::imagepack_size`]: ../utz_build_cli/cmd/imagepack_size/index.html
//! [`cmd::pip_bench`]: ../utz_build_cli/cmd/pip_bench/index.html
//! [`cmd::polygrid_probe`]: ../utz_build_cli/cmd/polygrid_probe/index.html
//! [`cmd::quant_clean`]: ../utz_build_cli/cmd/quant_clean/index.html
//! [`cmd::quant_size`]: ../utz_build_cli/cmd/quant_size/index.html
//! [`cmd::rdp_sweep`]: ../utz_build_cli/cmd/rdp_sweep/index.html
//! [`cmd::roundtrip`]: ../utz_build_cli/cmd/roundtrip/index.html
//! [`cmd::size_table`]: ../utz_build_cli/cmd/size_table/index.html
//! [`cmd::visualize`]: ../utz_build_cli/cmd/visualize/index.html
//! [`cmd::whittle`]: ../utz_build_cli/cmd/whittle/index.html
//! [`cmd::window_sweep`]: ../utz_build_cli/cmd/window_sweep/index.html

pub mod cmd;
