// Measurement backlog (PLAN.md §15): grid size × P(PIP) × memory with the
// *real* grid + interned-CSR builder (grid.rs), replacing gridsweep's crude
// border-cell estimate. For each cell size: border-cell fraction, sampled
// P(PIP) over uniform lon/lat points, unique interned lists, and the memory
// split (primary array vs CSR side table), dominant-first ordering as decided.
//
// usage: utz-build csr-sweep [eps_m]

use utz_build::grid::{self, Order};
use utz_build::topo;

const DEGS: [f64; 5] = [1.0, 2.0, 3.0, 5.0, 10.0];
const NPTS: usize = 200_000;

#[derive(clap::Args)]
pub struct Args {
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
}

pub fn run(a: &Args) -> anyhow::Result<()> {
    let eps_m = a.eps_m;
    let pts = gen_pts(NPTS);

    for ds in ["now", "1970"] {
        let feats = utz_build::load(ds)?;
        let out = topo::encode_topology(&feats, eps_m / 111_320.0);
        let areas = grid::feat_areas(&out.simplified);
        println!("{} eps={eps_m}m, {} features, dominant-first CSR, {NPTS} sample points",
            ds.to_uppercase(), out.simplified.len());
        println!("{:>4}{:>9}{:>9}{:>10}{:>9}{:>7}{:>8}{:>11}{:>11}{:>11}",
            "deg", "cells", "border", "border%", "P(PIP)", "lists", "ids", "primary", "side", "total");
        println!("{}", "-".repeat(89));

        for deg in DEGS {
            // keep subcell resolution ~0.25° regardless of cell size
            let sub = ((deg * 4.0).round() as usize).max(2);
            let g = grid::build(&out.simplified, deg, sub);
            let csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);

            let total = g.ncols * g.nrows;
            let border = csr.primary.iter().filter(|&&p| p & 0x8000 != 0 && p != 0x7FFF).count();
            let hits = pts.iter().filter(|&&(lon, lat)| {
                let c = (((lon + 180.0) / deg) as isize).clamp(0, g.ncols as isize - 1) as usize;
                let r = (((lat + 90.0) / deg) as isize).clamp(0, g.nrows as isize - 1) as usize;
                let p = csr.primary[r * g.ncols + c];
                p & 0x8000 != 0 && p != 0x7FFF
            }).count();

            let primary_b = csr.primary.len() * 2;
            let side_b = (csr.list_offsets.len() + csr.list_ids.len()) * 2;
            assert!(csr.uniq_lists < 0x7FFF, "list index overflows the 15-bit tag at {deg}°");
            assert!(u16::try_from(csr.list_ids.len()).is_ok(), "list_offsets u16 overflow at {deg}°");
            println!("{:>4}{:>9}{:>9}{:>9.1}%{:>8.1}%{:>7}{:>8}{:>8.1} KB{:>8.1} KB{:>8.1} KB",
                deg, total, border,
                100.0 * border as f64 / total as f64,
                100.0 * hits as f64 / NPTS as f64,
                csr.uniq_lists, csr.list_ids.len(),
                primary_b as f64 / 1024.0, side_b as f64 / 1024.0,
                (primary_b + side_b) as f64 / 1024.0);
        }
        println!();
    }
    Ok(())
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || { lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}
