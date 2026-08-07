//! Sweeps grid size × P(PIP) × memory with the *real* grid + interned-CSR
//! builder ([`utz_encode::grid`]), replacing gridsweep's crude border-cell
//! estimate. For each cell size it reports the border-cell fraction, the
//! sampled P(PIP) over uniform lon/lat points, the unique interned lists,
//! and the memory split (primary array vs CSR side table), with
//! dominant-first ordering as decided.
//!
//! ```text
//! utz_dev_cli csr-sweep [epsilon_m]
//! ```
//!
//! [`utz_encode::grid`]: ../utz_encode/grid/index.html

use utz_encode::grid::{self, Order};
use utz_encode::topo;

const DEGS: [f64; 5] = [1.0, 2.0, 3.0, 5.0, 10.0];
const NPTS: usize = 200_000;

#[derive(clap::Args)]
pub struct Args {
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    epsilon_m: f64,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
///
/// # Panics
/// The command panics if a cell size overflows the 16-bit CSR encoding: a
/// unique-list index past the 15-bit tag, or a `list_ids` length past
/// `u16`.
pub fn run(args: &Args) -> utz_build::Result<()> {
    let epsilon_m = args.epsilon_m;
    let points = gen_pts(NPTS);

    for dataset in ["now", "1970"] {
        let features = utz_build::load(dataset)?;
        let out = topo::encode_topology(&features, epsilon_m / utz_common::METERS_PER_DEG);
        let areas = grid::feat_areas(&out.simplified);
        println!(
            "{} epsilon={epsilon_m}m, {} features, dominant-first CSR, {NPTS} sample points",
            dataset.to_uppercase(),
            out.simplified.len()
        );
        println!(
            "{:>4}{:>9}{:>9}{:>10}{:>9}{:>7}{:>8}{:>11}{:>11}{:>11}",
            "deg",
            "cells",
            "border",
            "border%",
            "P(PIP)",
            "lists",
            "ids",
            "primary",
            "side",
            "total"
        );
        println!("{}", "-".repeat(89));

        for deg in DEGS {
            // keep subcell resolution ~0.25° regardless of cell size
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "deg ≤ 10 → tiny positive integer"
            )]
            let subcells = ((deg * 4.0).round() as usize).max(2);
            let grid = grid::build(&out.simplified, deg, subcells);
            let csr = grid::intern_csr(&grid, Order::CellDominantFirst, &areas);

            let total = grid.ncols() * grid.nrows();
            let border = csr
                .primary
                .iter()
                .filter(|&&tag| {
                    matches!(
                        utz_common::CellTag::from_cell(tag),
                        utz_common::CellTag::Border(_)
                    )
                })
                .count();
            let hits = points
                .iter()
                .filter(|&&(lon, lat)| {
                    let (row, col) =
                        utz_common::grid_cell(lon, lat, deg, grid.ncols(), grid.nrows());
                    let tag = csr.primary[row * grid.ncols() + col];
                    matches!(
                        utz_common::CellTag::from_cell(tag),
                        utz_common::CellTag::Border(_)
                    )
                })
                .count();

            let primary_bytes = csr.primary.len() * 2;
            let side_bytes = (csr.list_offsets.len() + csr.list_ids.len()) * 2;
            assert!(
                csr.uniq_lists < usize::from(utz_common::NO_ZONE),
                "list index overflows the 15-bit tag at {deg}°"
            );
            assert!(
                u16::try_from(csr.list_ids.len()).is_ok(),
                "list_offsets u16 overflow at {deg}°"
            );
            #[expect(
                clippy::cast_precision_loss,
                reason = "cell/hit counts and CSR byte sizes ≪ 2^53; % and KB display"
            )]
            let (border_pct, pip_pct, primary_kb, side_kb, total_kb) = (
                100.0 * border as f64 / total as f64,
                100.0 * hits as f64 / NPTS as f64,
                primary_bytes as f64 / 1024.0,
                side_bytes as f64 / 1024.0,
                (primary_bytes + side_bytes) as f64 / 1024.0,
            );
            println!(
                "{:>4}{:>9}{:>9}{border_pct:>9.1}%{pip_pct:>8.1}%{:>7}{:>8}{primary_kb:>8.1} KB{side_kb:>8.1} KB{total_kb:>8.1} KB",
                deg,
                total,
                border,
                csr.uniq_lists,
                csr.list_ids.len()
            );
        }
        println!();
    }
    Ok(())
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(utz_common::POINT_SEED, n)
}
