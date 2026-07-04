//! μTZ build & measurement CLI. Each subcommand lives in its own module
//! under [`cmd`] (these were formerly cargo examples).

use clap::Parser;

mod cmd;

#[derive(Parser)]
#[command(name = "utz-build", version, about = "μTZ build & measurement toolbox")]
enum Cmd {
    /// Generate the webdist viewer (static page + per-dataset binary blobs)
    Visualize(cmd::visualize::Args),
    /// Encode a .utz container to disk (bench-cli / firmware input)
    Encode(cmd::encode::Args),
    /// Misassigned area/population of simplified topologies vs raw arcs
    Accuracy(cmd::accuracy::Args),
    /// Uniform vs population-weighted simplification: verts by density band
    DensityCompare(cmd::density_compare::Args),
    /// Spot-check the GHS-POP ingest (downloads ~460 MB once)
    DensityProbe(cmd::density_probe::Args),
    /// End-to-end container roundtrip: encode, decode, validate vs linear PIP
    Roundtrip(cmd::roundtrip::Args),
    /// Full-container size table: eps × quant × codec
    SizeTable(cmd::size_table::Args),
    /// Arc-store encoding shootout (delta+varint vs abs-fixed)
    QuantSize(cmd::quant_size::Args),
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
    /// Hand-rolled i64 PIP vs geo vs geometry-rs: correctness + speed
    PipBench(cmd::pip_bench::Args),
    /// geo integer PIP vs f64 PIP agreement (i32 overflow check)
    Geoquant(cmd::geoquant::Args),
    /// Antimeridian scan: is TZBB already split at ±180°?
    Amscan(cmd::amscan::Args),
    /// Legacy fgb lookup benchmark (R-tree vs full scan vs custom)
    Bench(cmd::bench::Args),
}

fn main() -> anyhow::Result<()> {
    match Cmd::parse() {
        Cmd::Visualize(a) => cmd::visualize::run(a),
        Cmd::Encode(a) => cmd::encode::run(a),
        Cmd::Accuracy(a) => cmd::accuracy::run(a),
        Cmd::DensityCompare(a) => cmd::density_compare::run(a),
        Cmd::DensityProbe(a) => cmd::density_probe::run(a),
        Cmd::Roundtrip(a) => cmd::roundtrip::run(a),
        Cmd::SizeTable(a) => cmd::size_table::run(a),
        Cmd::QuantSize(a) => cmd::quant_size::run(a),
        Cmd::RdpSweep(a) => cmd::rdp_sweep::run(a),
        Cmd::CsrSweep(a) => cmd::csr_sweep::run(a),
        Cmd::Gridsweep(a) => cmd::gridsweep::run(a),
        Cmd::Grid2mem(a) => cmd::grid2mem::run(a),
        Cmd::GridBench(a) => cmd::grid_bench::run(a),
        Cmd::DominantCost(a) => cmd::dominant_cost::run(a),
        Cmd::PipBench(a) => cmd::pip_bench::run(a),
        Cmd::Geoquant(a) => cmd::geoquant::run(a),
        Cmd::Amscan(a) => cmd::amscan::run(a),
        Cmd::Bench(a) => cmd::bench::run(a),
    }
}
