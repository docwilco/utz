//! μTZ build & measurement CLI: the `utz-build-cli` binary.
//!
//! Not to be confused with the `utz-build` *library*: that reader-free
//! crate is what build scripts depend on. The split keeps build scripts
//! light: this CLI pulls in the runtime reader (`utz`) and measurement
//! dependencies that a `build.rs` never needs. (This doc surface is the
//! package's lib target; the bin target is `doc = false` so the two do
//! not race for the `utz_build_cli` doc directory.)
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

pub mod cmd;
