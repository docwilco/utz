//! Grid-size sweep (1..=20 deg). For each size: total cells, "border"
//! cells (a tz boundary edge passes through → lookup needs PIP), interior
//! cells (single zone → O(1)), the fraction of area-uniform lookups that
//! hit a border cell, and a memory estimate.
//!
//! ```text
//! utz_dev_cli gridsweep [ds]
//! ```

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
}

/// # Errors
/// Dataset load/parse failure.
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
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ceil(360/deg) is a small positive integer"
        )]
        let ncols = ((360.0 / deg_f64).ceil()) as usize;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ceil(180/deg) is a small positive integer"
        )]
        let nrows = ((180.0 / deg_f64).ceil()) as usize;
        let total = ncols * nrows;
        let mut border = vec![false; total];
        let cell = |lon: f64, lat: f64| -> usize {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_possible_wrap,
                reason = "cell index, fraction dropped then clamped"
            )]
            let col = (((lon + 180.0) / deg_f64) as isize).clamp(0, ncols as isize - 1) as usize;
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_possible_wrap,
                reason = "cell index, fraction dropped then clamped"
            )]
            let row = (((lat + 90.0) / deg_f64) as isize).clamp(0, nrows as isize - 1) as usize;
            row * ncols + col
        };
        for ring in &rings {
            let ring_len = ring.len();
            for i in 0..ring_len {
                let (x0, y0) = ring[i];
                let (x1, y1) = ring[(i + 1) % ring_len];
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "edge span in cells is small and non-negative"
                )]
                let span = (((x1 - x0).abs()).max((y1 - y0).abs()) / deg_f64 * 2.0).ceil() as usize;
                let steps = span.max(1);
                for step in 0..=steps {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "step ≤ steps = small per-edge cell span; exact"
                    )]
                    let t = step as f64 / steps as f64;
                    border[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)] = true;
                }
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
