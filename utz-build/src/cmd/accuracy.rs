//! Accuracy of simplified topologies vs the raw (ε=0) arcs.
//!
//! Every simplifier in the menu keeps a *subset* of the original vertices, so
//! each output segment covers a contiguous run of raw vertices and the
//! misassigned region decomposes exactly into "pockets" between the raw
//! sub-chain and its shortcut (split where the chain crosses the shortcut
//! line). Per config this reports:
//!   - max deviation (m, same flat 111 320 m/deg convention as `eps_m`)
//!   - misassigned area (km², sum of |pocket|)
//!   - misassigned population (people: pocket area × GHS-POP density at the
//!     pocket — pockets are ≤ ε wide, far below the 4′ grid, so one sample
//!     per pocket is essentially exact)
//!
//!     utz-build accuracy [ds] [`eps_m`] [`w_min`] [rdp|vw|ii]

use utz_build::density::DensityGrid;
use utz_build::topo::{self, Simplify, Topology};
use utz_simplify::DensityWeight;

const KM_PER_DEG: f64 = 111.32;

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// weighted-floor multiplier at max density
    #[arg(default_value_t = 0.052)]
    w_min: f64,
    /// simplification algorithm: rdp|vw|ii
    #[arg(default_value = "rdp")]
    algo: String,
}

pub fn run(a: Args) -> utz_build::Result<()> {
    let (ds, eps_m, w_min, algo_key) = (a.ds, a.eps_m, a.w_min, a.algo);
    let algo = |eps_deg: f64| -> Simplify {
        match algo_key.as_str() {
            "rdp" => Simplify::Rdp { eps: eps_deg },
            "vw" => Simplify::Visvalingam { min_area: eps_deg * eps_deg },
            "ii" => Simplify::ImaiIri { eps: eps_deg },
            k => panic!("unknown algo {k:?}: use rdp|vw|ii"),
        }
    };

    let feats = utz_build::load(&ds)?;
    let grid = DensityGrid::load(&utz_build::cache_dir())?;
    let t0 = topo::build_topology(&feats, 0.0);
    let model = DensityWeight::new(w_min);

    let e = eps_m / 111_320.0;
    let configs: Vec<(String, Topology)> = vec![
        (format!("uniform ε{eps_m}"), topo::build_topology_algo(&feats, algo(e))),
        (format!("uniform ε{}", eps_m / 2.0), topo::build_topology_algo(&feats, algo(e / 2.0))),
        (
            format!("weighted ε{eps_m}×{w_min}"),
            topo::build_topology_weighted(&feats, algo(e), &|a, b| model.weight(grid.max_along(a, b))),
        ),
    ];

    println!("{ds} · {algo_key} · misassignment vs raw ε=0 arcs\n");
    println!("{:>22} {:>9} {:>10} {:>12} {:>14}", "config", "verts", "max dev", "misassigned", "misassigned");
    println!("{:>22} {:>9} {:>10} {:>12} {:>14}", "", "", "(m)", "area (km²)", "pop (people)");
    for (name, t) in &configs {
        let verts: usize = t.arc_coords.iter().map(std::vec::Vec::len).sum();
        let m = measure(&t0, t, &grid);
        println!(
            "{name:>22} {verts:>9} {:>10.1} {:>12.1} {:>14.0}",
            m.max_dev_deg * 111_320.0,
            m.area_km2,
            m.people
        );
    }
    Ok(())
}

#[derive(Default)]
struct Acc {
    max_dev_deg: f64,
    area_km2: f64,
    people: f64,
}

fn measure(t0: &Topology, t: &Topology, grid: &DensityGrid) -> Acc {
    let mut acc = Acc::default();
    // arcs are cut before simplification, so the two topologies' arc lists
    // correspond 1:1 by index
    for (orig, simp) in t0.arc_coords.iter().zip(&t.arc_coords) {
        // simplified vertices are bit-exact copies of raw ones — walk-match
        // them back to raw indices
        let mut idx = Vec::with_capacity(simp.len());
        let mut j = 0;
        for &p in simp {
            while orig[j] != p {
                j += 1;
            }
            idx.push(j);
            j += 1;
        }
        for w in idx.windows(2) {
            pocket_scan(&orig[w[0]..=w[1]], grid, &mut acc);
        }
    }
    acc
}

/// Decompose the region between a raw sub-chain and its shortcut
/// (`chain.first()` → `chain.last()`) into pockets, splitting the anchored
/// shoelace accumulation wherever the chain crosses the shortcut line.
fn pocket_scan(chain: &[(f64, f64)], grid: &DensityGrid, acc: &mut Acc) {
    let (start, end) = (chain[0], *chain.last().unwrap());
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let cross_anchored = |from: (f64, f64), to: (f64, f64)| {
        (from.0 - start.0) * (to.1 - start.1) - (from.1 - start.1) * (to.0 - start.0)
    };
    let side = |point: (f64, f64)| dx * (point.1 - start.1) - dy * (point.0 - start.0);
    let len2 = dx * dx + dy * dy;

    let flush = |area_deg2: f64, lonc: f64, latc: f64, acc: &mut Acc| {
        let km2 = area_deg2.abs() * KM_PER_DEG * KM_PER_DEG * latc.to_radians().cos();
        acc.area_km2 += km2;
        acc.people += km2 * grid.sample(lonc, latc);
    };

    // signed pocket accumulator + running centroid of its chain vertices
    let (mut pocket, mut clon, mut clat, mut npts) = (0.0, start.0, start.1, 1.0);
    for seg_idx in 0..chain.len() - 1 {
        let (curr, next) = (chain[seg_idx], chain[seg_idx + 1]);
        // max deviation (perpendicular distance to the clamped shortcut)
        if seg_idx > 0 {
            let dist2 = if len2 == 0.0 {
                (curr.0 - start.0).powi(2) + (curr.1 - start.1).powi(2)
            } else {
                let frac = (((curr.0 - start.0) * dx + (curr.1 - start.1) * dy) / len2).clamp(0.0, 1.0);
                (curr.0 - start.0 - frac * dx).powi(2) + (curr.1 - start.1 - frac * dy).powi(2)
            };
            acc.max_dev_deg = acc.max_dev_deg.max(dist2.sqrt());
        }
        let (side_curr, side_next) = (side(curr), side(next));
        if len2 > 0.0 && side_curr * side_next < 0.0 {
            // chain crosses the shortcut line: split the step at the crossing
            let cross_frac = side_curr / (side_curr - side_next);
            let crossing = (curr.0 + cross_frac * (next.0 - curr.0), curr.1 + cross_frac * (next.1 - curr.1));
            pocket += cross_anchored(curr, crossing) / 2.0;
            flush(pocket, clon / npts, clat / npts, acc);
            pocket = cross_anchored(crossing, next) / 2.0;
            (clon, clat, npts) = (crossing.0 + next.0, crossing.1 + next.1, 2.0);
        } else {
            pocket += cross_anchored(curr, next) / 2.0;
            clon += next.0;
            clat += next.1;
            npts += 1.0;
        }
    }
    flush(pocket, clon / npts, clat / npts, acc);
}
