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

pub fn run(args: &Args) -> utz_build::Result<()> {
    let eps_m = args.eps_m;
    let pts = gen_pts(NPTS);

    for dataset in ["now", "1970"] {
        let feats = utz_build::load(dataset)?;
        let out = topo::encode_topology(&feats, eps_m / 111_320.0);
        let areas = grid::feat_areas(&out.simplified);
        println!("{} eps={eps_m}m, {} features, dominant-first CSR, {NPTS} sample points",
            dataset.to_uppercase(), out.simplified.len());
        println!("{:>4}{:>9}{:>9}{:>10}{:>9}{:>7}{:>8}{:>11}{:>11}{:>11}",
            "deg", "cells", "border", "border%", "P(PIP)", "lists", "ids", "primary", "side", "total");
        println!("{}", "-".repeat(89));

        for deg in DEGS {
            // keep subcell resolution ~0.25° regardless of cell size
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "deg ≤ 10 → tiny positive integer")]
            let sub = ((deg * 4.0).round() as usize).max(2);
            let grid = grid::build(&out.simplified, deg, sub);
            let csr = grid::intern_csr(&grid, Order::CellDominantFirst, &areas);

            let total = grid.ncols * grid.nrows;
            let border = csr.primary.iter().filter(|&&tag| tag & 0x8000 != 0 && tag != 0x7FFF).count();
            let hits = pts.iter().filter(|&&(lon, lat)| {
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, reason = "cell index, fraction dropped then clamped")]
                let col = (((lon + 180.0) / deg) as isize).clamp(0, grid.ncols as isize - 1) as usize;
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, reason = "cell index, fraction dropped then clamped")]
                let row = (((lat + 90.0) / deg) as isize).clamp(0, grid.nrows as isize - 1) as usize;
                let tag = csr.primary[row * grid.ncols + col];
                tag & 0x8000 != 0 && tag != 0x7FFF
            }).count();

            let primary_bytes = csr.primary.len() * 2;
            let side_bytes = (csr.list_offsets.len() + csr.list_ids.len()) * 2;
            assert!(csr.uniq_lists < 0x7FFF, "list index overflows the 15-bit tag at {deg}°");
            assert!(u16::try_from(csr.list_ids.len()).is_ok(), "list_offsets u16 overflow at {deg}°");
            #[expect(clippy::cast_precision_loss, reason = "cell/hit counts and CSR byte sizes ≪ 2^53; % and KB display")]
            let (border_pct, pip_pct, primary_kb, side_kb, total_kb) = (
                100.0 * border as f64 / total as f64,
                100.0 * hits as f64 / NPTS as f64,
                primary_bytes as f64 / 1024.0, side_bytes as f64 / 1024.0,
                (primary_bytes + side_bytes) as f64 / 1024.0,
            );
            println!("{:>4}{:>9}{:>9}{border_pct:>9.1}%{pip_pct:>8.1}%{:>7}{:>8}{primary_kb:>8.1} KB{side_kb:>8.1} KB{total_kb:>8.1} KB",
                deg, total, border,
                csr.uniq_lists, csr.list_ids.len());
        }
        println!();
    }
    Ok(())
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
