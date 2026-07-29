//! μTZ build & measurement CLI: the `utz-build` binary.
//!
//! The binary is named `utz-build` but is built from this `utz-build-cli`
//! package; the `utz-build` *library* (the `utz_build` build-dependency
//! that build scripts use) is a separate, reader-free crate. The split
//! keeps build scripts light: this CLI pulls in the runtime reader (`utz`)
//! and measurement dependencies that a `build.rs` never needs.
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

use clap::Parser;

pub mod cmd;

#[derive(Parser)]
#[command(name = "utz-build", version, about = "μTZ build & measurement toolbox")]
enum Cmd {
    /// Generate the webdist viewer (static page + per-dataset binary blobs)
    Visualize(cmd::visualize::Args),
    /// Generate a .utz asset to disk (the custom-tier CLI;
    /// also feeds bench-cli / firmware)
    #[command(visible_alias = "encode")]
    Gen(cmd::encode::Args),
    /// Misassigned area/population of simplified topologies vs raw arcs
    Accuracy(cmd::accuracy::Args),
    /// Uniform vs population-weighted simplification: verts by density band
    DensityCompare(cmd::density_compare::Args),
    /// Spot-check the GHS-POP ingest (downloads ~460 MB once)
    DensityProbe(cmd::density_probe::Args),
    /// End-to-end asset roundtrip: encode, decode, validate vs linear PIP
    Roundtrip(cmd::roundtrip::Args),
    /// Full-asset size table: eps × quant × codec
    SizeTable(cmd::size_table::Args),
    /// Per-stage pipeline size reduction on the preset recipes
    Whittle(cmd::whittle::Args),
    /// Ratio vs window/dict size per codec + measured peak decode RAM
    WindowSweep(cmd::window_sweep::Args),
    /// Arc-store encoding shootout (delta+varint vs abs-fixed)
    QuantSize(cmd::quant_size::Args),
    /// Quantization-artifact report: mangled rings before/after cleanup
    QuantClean(cmd::quant_clean::Args),
    /// Topology-aware RDP sweep: size + lookup accuracy per eps
    RdpSweep(cmd::rdp_sweep::Args),
    /// Grid size × P(PIP) × memory with the real interned-CSR builder
    CsrSweep(cmd::csr_sweep::Args),
    /// Crude grid-size sweep (border cells / P(PIP) / memory estimate)
    Gridsweep(cmd::gridsweep::Args),
    /// Exact memory of a grid at one cell size, across layouts
    Grid2mem(cmd::grid2mem::Args),
    /// Real grid lookup bench: interned-CSR prefilter vs linear scan
    GridBench(cmd::grid_bench::Args),
    /// Candidate-list ordering cost/benefit (id-sorted vs dominant-first)
    DominantCost(cmd::dominant_cost::Args),
    /// μTZ's i64 PIP vs geo vs geometry-rs: correctness + speed
    PipBench(cmd::pip_bench::Args),
    /// geo integer PIP vs f64 PIP agreement (i32 overflow check)
    Geoquant(cmd::geoquant::Args),
    /// Antimeridian scan: is TZBB already split at ±180°?
    Amscan(cmd::amscan::Args),
    /// Fixed-width arc-store size vs delta+varint (from codec-none assets)
    FixedwidthSize(cmd::fixedwidth_size::Args),
    /// Poly-granular grid vs per-poly bboxes probe
    PolygridProbe(cmd::polygrid_probe::Args),
    /// Packed `FullRings` coords vs general compression (geom=2 assets)
    ImagepackSize(cmd::imagepack_size::Args),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> utz_build::Result<()> {
    match Cmd::parse() {
        Cmd::Visualize(a) => cmd::visualize::run(a),
        Cmd::Gen(a) => cmd::encode::run(a),
        Cmd::Accuracy(a) => cmd::accuracy::run(a),
        Cmd::DensityCompare(a) => cmd::density_compare::run(a),
        Cmd::DensityProbe(a) => cmd::density_probe::run(a),
        Cmd::Roundtrip(a) => cmd::roundtrip::run(a),
        Cmd::SizeTable(a) => cmd::size_table::run(a),
        Cmd::Whittle(a) => cmd::whittle::run(&a),
        Cmd::WindowSweep(a) => cmd::window_sweep::run(&a),
        Cmd::QuantSize(a) => cmd::quant_size::run(a),
        Cmd::QuantClean(a) => cmd::quant_clean::run(&a),
        Cmd::RdpSweep(a) => cmd::rdp_sweep::run(a),
        Cmd::CsrSweep(a) => cmd::csr_sweep::run(&a),
        Cmd::Gridsweep(a) => cmd::gridsweep::run(a),
        Cmd::Grid2mem(a) => cmd::grid2mem::run(a),
        Cmd::GridBench(a) => cmd::grid_bench::run(a),
        Cmd::DominantCost(a) => cmd::dominant_cost::run(a),
        Cmd::PipBench(a) => cmd::pip_bench::run(a),
        Cmd::Geoquant(a) => cmd::geoquant::run(a),
        Cmd::Amscan(a) => cmd::amscan::run(a),
        Cmd::FixedwidthSize(a) => cmd::fixedwidth_size::run(&a),
        Cmd::PolygridProbe(a) => cmd::polygrid_probe::run(&a),
        Cmd::ImagepackSize(a) => cmd::imagepack_size::run(&a),
    }
}
