//! Reports quantization artifacts: how badly grid snapping mangles the
//! ring geometry (self-crossings, collinear self-overlaps, self-touches,
//! zero-area rings), and how much of that the [`utz_encode::clean`]
//! pass removes. Rings are assembled from the shared arcs exactly like the
//! encoder does; the measuring itself lives in `utz_encode`'s validate
//! module (shared with the viewer's problems panel). `--locate` lists each
//! surviving crossing/overlap as a live-viewer URL.
//!
//! ```text
//! utz_dev_cli quant-clean [ds] [eps_m] [qbits...] [--locate]
//! ```
//!
//! [`utz_encode::clean`]: ../utz_encode/clean/index.html

use utz_build::{ensure, Error};
use utz_encode::clean::{self, CleanStats};
use utz_encode::topo;
use utz_encode::validate::{self, Bad, Kind};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// The quantization widths to report (16/24/32).
    #[arg(default_values_t = [16u32, 24])]
    qbits: Vec<u32>,
    /// Lists each post-cleanup crossing/overlap as a zone plus live-viewer
    /// URL.
    #[arg(long)]
    locate: bool,
    /// The live-viewer base URL for --locate links.
    #[arg(long, default_value = "https://docwilco.github.io/utz/live/index.html")]
    viewer: String,
}

/// # Errors
/// The command fails on a dataset load/parse failure, or on a qbits value
/// other than 16/24/32.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: &Args) -> utz_build::Result<()> {
    let features = utz_build::load(&args.ds)?;
    let topology = topo::build_topology(&features, args.eps_m / 111_320.0);
    println!(
        "{} · RDP ε {} m · {} arcs, {} rings\n",
        args.ds,
        args.eps_m,
        topology.arc_coords.len(),
        topology.ring_refs.len()
    );

    for &quant_bits in &args.qbits {
        ensure!(
            matches!(quant_bits, 16 | 24 | 32),
            Error::Msg(format!("qbits must be 16/24/32 (got {quant_bits})"))
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64"
        )]
        let qmax = ((1u64 << (quant_bits - 1)) - 1) as f64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "|coord·qmax| ≤ qmax < 2^31"
        )]
        let quant = |arc: &Vec<(f64, f64)>| -> Vec<(i32, i32)> {
            arc.iter()
                .map(|&(x, y)| {
                    (
                        ((x / 180.0 * qmax).round()) as i32,
                        ((y / 90.0 * qmax).round()) as i32,
                    )
                })
                .collect()
        };

        // before: what the encoder used to ship (consecutive-dup collapse only)
        let raw: Vec<Vec<(i32, i32)>> = topology
            .arc_coords
            .iter()
            .map(|arc| {
                let mut quantized = quant(arc);
                quantized.dedup();
                quantized
            })
            .collect();
        let before = validate::measure(
            topology
                .ring_refs
                .iter()
                .map(|ring_ref| clean::ring_coords_q(ring_ref, &raw)),
        );

        // after: per-arc clean + degenerate-ring drop (what the encoder ships now)
        let mut clean_stats = CleanStats::default();
        let cleaned: Vec<Vec<(i32, i32)>> = topology
            .arc_coords
            .iter()
            .map(|arc| {
                let mut quantized = quant(arc);
                let closed = arc.len() > 1 && arc.first() == arc.last();
                clean::clean_arc(&mut quantized, closed, &mut clean_stats);
                quantized
            })
            .collect();
        let (ring_refs, _, cleaned_arcs) = clean::drop_degenerate_rings(
            &topology.ring_refs,
            &topology.structure,
            cleaned,
            &mut clean_stats,
        );
        let after = validate::measure(
            ring_refs
                .iter()
                .map(|ring_ref| clean::ring_coords_q(ring_ref, &cleaned_arcs)),
        );

        println!("i{quant_bits}");
        let row = |tag: &str, bad: &Bad| {
            println!(
                "  {tag:<7} verts {:>8}  cross {:>5}  overlap {:>5}  touch {:>5}  degenerate rings {:>4}",
                bad.verts, bad.crossings, bad.overlaps, bad.touches, bad.degenerate
            );
        };
        row("before:", &before);
        println!(
            "  clean:  dups {}  spikes {}  collinear {}  rings dropped {} (polys {}, arcs {})",
            clean_stats.dups,
            clean_stats.spikes,
            clean_stats.collinear,
            clean_stats.rings_dropped,
            clean_stats.polys_dropped,
            clean_stats.arcs_dropped
        );
        row("after:", &after);

        if args.locate {
            // same pipeline, but with owner mapping + dedup by location: a
            // spot on a shared border shows up once per owning ring — group
            // the zones per location instead of repeating the URL
            let mut spots: std::collections::BTreeMap<
                (String, &str),
                std::collections::BTreeSet<&str>,
            > = std::collections::BTreeMap::new();
            for problem in validate::find_problems(&topology, &topology.arc_coords, quant_bits) {
                let kind = match problem.kind {
                    Kind::Cross => "cross",
                    Kind::Overlap => "overlap",
                };
                let tzid = features[problem.feat].tzid.as_deref().unwrap_or("?");
                spots
                    .entry((format!("{:.5},{:.5}", problem.lat, problem.lon), kind))
                    .or_default()
                    .insert(tzid);
            }
            for ((at, kind), tzids) in &spots {
                let zones = tzids.iter().copied().collect::<Vec<_>>().join(" + ");
                println!(
                    "    {kind:<7} {zones:<44} {}#m={at},15&l0={},rdp,{},i{quant_bits},off",
                    args.viewer, args.ds, args.eps_m
                );
            }
        }
        println!();
    }
    Ok(())
}
