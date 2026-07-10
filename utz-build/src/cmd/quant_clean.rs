//! Quantization-artifact report: how badly does grid snapping mangle the
//! ring geometry (self-crossings, collinear self-overlaps, self-touches,
//! zero-area rings), and how much of that does the clean.rs pass remove.
//! Rings are assembled from the shared arcs exactly like the encoder does;
//! the measuring itself lives in utz-encode's validate module (shared with
//! the viewer's problems panel). `--locate` lists each surviving
//! crossing/overlap as a live-viewer URL.
//!
//!     utz-build quant-clean [ds] [eps_m] [qbits...] [--locate]

use utz_build::clean::{self, CleanStats};
use utz_build::topo;
use utz_build::validate::{self, Bad, Kind};
use utz_build::{ensure, Error};

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// quantization widths to report (16/24/32)
    #[arg(default_values_t = [16u32, 24])]
    qbits: Vec<u32>,
    /// list each post-cleanup crossing/overlap as zone + live-viewer URL
    #[arg(long)]
    locate: bool,
    /// live viewer base for --locate links
    #[arg(long, default_value = "https://docwilco.github.io/utz/live/index.html")]
    viewer: String,
}

pub fn run(a: &Args) -> utz_build::Result<()> {
    let feats = utz_build::load(&a.ds)?;
    let t = topo::build_topology(&feats, a.eps_m / 111_320.0);
    println!("{} · RDP ε {} m · {} arcs, {} rings\n", a.ds, a.eps_m, t.arc_coords.len(), t.ring_refs.len());

    for &qbits in &a.qbits {
        ensure!(matches!(qbits, 16 | 24 | 32), Error::Msg(format!("qbits must be 16/24/32 (got {qbits})")));
        #[expect(clippy::cast_precision_loss, reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64")]
        let qmax = ((1u64 << (qbits - 1)) - 1) as f64;
        #[expect(clippy::cast_possible_truncation, reason = "|coord·qmax| ≤ qmax < 2^31")]
        let quant = |a: &Vec<(f64, f64)>| -> Vec<(i32, i32)> {
            a.iter()
                .map(|&(x, y)| (((x / 180.0 * qmax).round()) as i32, ((y / 90.0 * qmax).round()) as i32))
                .collect()
        };

        // before: what the encoder used to ship (consecutive-dup collapse only)
        let raw: Vec<Vec<(i32, i32)>> = t.arc_coords.iter().map(|a| {
            let mut q = quant(a);
            q.dedup();
            q
        }).collect();
        let before = validate::measure(t.ring_refs.iter().map(|r| clean::ring_coords_q(r, &raw)));

        // after: per-arc clean + degenerate-ring drop (what the encoder ships now)
        let mut cst = CleanStats::default();
        let cleaned: Vec<Vec<(i32, i32)>> = t.arc_coords.iter().map(|a| {
            let mut q = quant(a);
            let closed = a.len() > 1 && a.first() == a.last();
            clean::clean_arc(&mut q, closed, &mut cst);
            q
        }).collect();
        let (ring_refs, _, arcs) = clean::drop_degenerate_rings(&t.ring_refs, &t.structure, cleaned, &mut cst);
        let after = validate::measure(ring_refs.iter().map(|r| clean::ring_coords_q(r, &arcs)));

        println!("i{qbits}");
        let row = |tag: &str, b: &Bad| {
            println!(
                "  {tag:<7} verts {:>8}  cross {:>5}  overlap {:>5}  touch {:>5}  degenerate rings {:>4}",
                b.verts, b.crossings, b.overlaps, b.touches, b.degenerate
            );
        };
        row("before:", &before);
        println!(
            "  clean:  dups {}  spikes {}  collinear {}  rings dropped {} (polys {}, arcs {})",
            cst.dups, cst.spikes, cst.collinear, cst.rings_dropped, cst.polys_dropped, cst.arcs_dropped
        );
        row("after:", &after);

        if a.locate {
            // same pipeline, but with owner mapping + dedup by location: a
            // spot on a shared border shows up once per owning ring — group
            // the zones per location instead of repeating the URL
            let mut spots: std::collections::BTreeMap<(String, &str), std::collections::BTreeSet<&str>> =
                std::collections::BTreeMap::new();
            for p in validate::find_problems(&t, &t.arc_coords, qbits) {
                let kind = match p.kind { Kind::Cross => "cross", Kind::Overlap => "overlap" };
                let tz = feats[p.feat].tzid.as_deref().unwrap_or("?");
                spots.entry((format!("{:.5},{:.5}", p.lat, p.lon), kind)).or_default().insert(tz);
            }
            for ((at, kind), tzs) in &spots {
                let zones = tzs.iter().copied().collect::<Vec<_>>().join(" + ");
                println!(
                    "    {kind:<7} {zones:<44} {}#m={at},15&l0={},rdp,{},i{qbits},off",
                    a.viewer, a.ds, a.eps_m
                );
            }
        }
        println!();
    }
    Ok(())
}
