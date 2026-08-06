// The utz_dev_cli binary: a thin dispatcher over utz_dev_cli::cmd.
// Crate docs live on the lib target (src/lib.rs); this bin target is
// doc = false so the two don't race for the utz_dev_cli doc directory.

use clap::Parser;

use utz_dev_cli::cmd;

#[derive(Parser)]
#[command(
    name = "utz_dev_cli",
    version,
    about = "μTZ development toolbox: measurement, bench, and viewer commands"
)]
enum Cmd {
    /// Generates the webdist viewer (static page + per-dataset binary
    /// blobs).
    Visualize(cmd::visualize::Args),
    /// Measures the misassigned area/population of simplified topologies
    /// vs raw arcs.
    Accuracy(cmd::accuracy::Args),
    /// Compares uniform vs population-weighted simplification, tabulating
    /// vertices by density band.
    DensityCompare(cmd::density_compare::Args),
    /// Spot-checks the GHS-POP ingest (downloads ~460 MB once).
    DensityProbe(cmd::density_probe::Args),
    /// Runs the end-to-end asset roundtrip: it encodes, decodes, and
    /// validates vs linear PIP.
    Roundtrip(cmd::roundtrip::Args),
    /// Prints the full-asset size table: epsilon × quant × codec.
    SizeTable(cmd::size_table::Args),
    /// Measures the per-stage pipeline size reduction on the preset
    /// recipes.
    Whittle(cmd::whittle::Args),
    /// Measures ratio vs window/dict size per codec, plus peak decode RAM.
    WindowSweep(cmd::window_sweep::Args),
    /// Runs the arc-store encoding shootout (delta+varint vs abs-fixed).
    QuantSize(cmd::quant_size::Args),
    /// Prints the quantization-artifact report (mangled rings before/after
    /// cleanup).
    QuantClean(cmd::quant_clean::Args),
    /// Runs the topology-aware RDP sweep (size and lookup accuracy per
    /// epsilon).
    RdpSweep(cmd::rdp_sweep::Args),
    /// Sweeps grid size × P(PIP) × memory with the real interned-CSR
    /// builder.
    CsrSweep(cmd::csr_sweep::Args),
    /// Runs the crude grid-size sweep (border cells / P(PIP) / memory
    /// estimate).
    Gridsweep(cmd::gridsweep::Args),
    /// Computes the exact memory of a grid at one cell size, across
    /// layouts.
    Grid2mem(cmd::grid2mem::Args),
    /// Runs the real grid lookup bench (interned-CSR prefilter vs linear
    /// scan).
    GridBench(cmd::grid_bench::Args),
    /// Measures the candidate-list ordering cost/benefit (id-sorted vs
    /// dominant-first).
    DominantCost(cmd::dominant_cost::Args),
    /// Races μTZ's i64 PIP vs geo vs geometry-rs for correctness and
    /// speed.
    PipBench(cmd::pip_bench::Args),
    /// Checks geo integer PIP vs f64 PIP agreement (the i32 overflow
    /// check).
    Geoquant(cmd::geoquant::Args),
    /// Scans the antimeridian: is TZBB already split at ±180°?
    Amscan(cmd::amscan::Args),
    /// Compares fixed-width arc-store size vs delta+varint (from
    /// codec-none assets).
    FixedwidthSize(cmd::fixedwidth_size::Args),
    /// Probes a poly-granular grid vs per-poly bboxes.
    PolygridProbe(cmd::polygrid_probe::Args),
    /// Compares packed `FullRings` coords vs general compression (geom=2
    /// assets).
    FullRingsSize(cmd::fullrings_size::Args),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> utz_build::Result<()> {
    match Cmd::parse() {
        Cmd::Visualize(args) => cmd::visualize::run(args),
        Cmd::Accuracy(args) => cmd::accuracy::run(args),
        Cmd::DensityCompare(args) => cmd::density_compare::run(args),
        Cmd::DensityProbe(args) => cmd::density_probe::run(args),
        Cmd::Roundtrip(args) => cmd::roundtrip::run(args),
        Cmd::SizeTable(args) => cmd::size_table::run(args),
        Cmd::Whittle(args) => cmd::whittle::run(&args),
        Cmd::WindowSweep(args) => cmd::window_sweep::run(&args),
        Cmd::QuantSize(args) => cmd::quant_size::run(args),
        Cmd::QuantClean(args) => cmd::quant_clean::run(&args),
        Cmd::RdpSweep(args) => cmd::rdp_sweep::run(args),
        Cmd::CsrSweep(args) => cmd::csr_sweep::run(&args),
        Cmd::Gridsweep(args) => cmd::gridsweep::run(args),
        Cmd::Grid2mem(args) => cmd::grid2mem::run(args),
        Cmd::GridBench(args) => cmd::grid_bench::run(args),
        Cmd::DominantCost(args) => cmd::dominant_cost::run(args),
        Cmd::PipBench(args) => cmd::pip_bench::run(args),
        Cmd::Geoquant(args) => cmd::geoquant::run(args),
        Cmd::Amscan(args) => cmd::amscan::run(args),
        Cmd::FixedwidthSize(args) => cmd::fixedwidth_size::run(&args),
        Cmd::PolygridProbe(args) => cmd::polygrid_probe::run(&args),
        Cmd::FullRingsSize(args) => cmd::fullrings_size::run(&args),
    }
}
