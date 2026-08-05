//! Sweeps the grid cell size over 1..=20 degrees. For each size it
//! reports the total cell count, the "border" cells (a tz boundary edge
//! passes through, so lookup needs PIP), the interior cells (a single
//! zone, so lookup is O(1)), the fraction of area-uniform lookups that
//! hit a border cell, and a memory estimate.
//!
//! ```text
//! utz_dev_cli gridsweep [ds]
//! ```

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
pub fn run(args: Args) -> utz_build::Result<()> {
    let dataset = args.ds;
    let features = utz_build::load(&dataset)?;
    let rings: Vec<Vec<(f64, f64)>> = features
        .iter()
        .flat_map(|feature| feature.polys.iter().flatten().cloned())
        .collect();
    let n_edges: usize = rings.iter().map(std::vec::Vec::len).sum();
    println!(
        "{}: {} rings, ~{} edges\n",
        dataset.to_uppercase(),
        rings.len(),
        n_edges
    );
    println!(
        "{:>4}{:>12}{:>12}{:>11}{:>13}{:>11}",
        "deg", "cells", "border", "interior", "P(PIP)", "grid mem"
    );
    println!("{}", "-".repeat(63));

    for deg in 1u32..=20 {
        let deg_f64 = f64::from(deg);
        let (ncols, nrows) = utz_encode::grid::grid_dims(deg_f64);
        let total = ncols * nrows;
        let mut border = vec![false; total];
        for ring in &rings {
            let ring_len = ring.len();
            for i in 0..ring_len {
                utz_encode::grid::walk_edge(
                    ring[i],
                    ring[(i + 1) % ring_len],
                    deg_f64,
                    &mut |lon, lat| {
                        let (row, col) = utz_common::grid_cell(lon, lat, deg_f64, ncols, nrows);
                        border[row * ncols + col] = true;
                    },
                );
            }
        }
        let border_count = border.iter().filter(|&&x| x).count();
        let interior = total - border_count;
        #[expect(
            clippy::cast_precision_loss,
            reason = "border_count ≤ total grid-cell count ≪ 2^53; percentage display"
        )]
        let p_pip = 100.0 * border_count as f64 / total as f64;
        // dense primary-zone u16 per cell + ~2 spillover u16 per border cell
        let mem = total * 2 + border_count * 4;
        println!(
            "{:>4}{:>12}{:>12}{:>11}{:>12.1}%{:>9} KB",
            deg,
            total,
            border_count,
            interior,
            p_pip,
            mem / 1024
        );
    }
    Ok(())
}
