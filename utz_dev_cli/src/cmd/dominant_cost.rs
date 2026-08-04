//! Dominant-first candidate-list ordering: how many extra unique lists /
//! KB does it cost vs id-sorted interning, and how much PIP early-exit
//! does each ordering actually buy?
//!
//! Orderings:
//!   id-sorted          — baseline, maximal interning
//!   area-desc          — global zone area descending; deterministic per set, so
//!                        interning is preserved *by construction* (verified here)
//!   cell-dominant-first — this cell's dominant zone first; best early-exit,
//!                        breaks interning (the cost being measured)
//!
//! Early-exit quality = fraction of owned subcells (0.25° at 2°) inside border
//! cells whose owner equals `list[0]` — i.e. P(first PIP hit) for area-uniform
//! lookups landing in border cells.
//!
//! ```text
//! utz_dev_cli dominant-cost [deg] [datasets...]
//! ```

use utz_encode::grid::{self, Order};

#[derive(clap::Args)]
pub struct Args {
    /// grid cell size in degrees
    #[arg(default_value_t = 2.0)]
    deg: f64,
    /// datasets: [land-]now|1970|all
    #[arg(default_values_t = [String::from("now"), String::from("1970")])]
    ds: Vec<String>,
}

/// # Errors
/// Dataset load/parse failure.
pub fn run(args: Args) -> utz_build::Result<()> {
    let (deg, datasets) = (args.deg, args.ds);
    for dataset in &datasets {
        let features = utz_build::load(dataset)?;
        let areas = grid::feat_areas(&features);
        let grid = grid::build(&features, deg, 8);
        let border = grid.sets.iter().filter(|set| set.len() > 1).count();
        println!(
            "{} @ {deg}°  ({} zones, {} border cells)",
            dataset.to_uppercase(),
            features.len(),
            border
        );
        println!(
            "{:<22}{:>12}{:>10}{:>12}{:>14}",
            "ordering", "uniq lists", "ids", "CSR bytes", "P(hit@[0])"
        );
        println!("{}", "-".repeat(70));

        let mut base_bytes = 0usize;
        for (name, order) in [
            ("id-sorted", Order::IdSorted),
            ("area-desc", Order::AreaDesc),
            ("cell-dominant-first", Order::CellDominantFirst),
        ] {
            let csr = grid::intern_csr(&grid, order, &areas);
            let hit = early_exit(&grid, &csr);
            if order == Order::IdSorted {
                base_bytes = csr.bytes();
            }
            let delta = csr.bytes().cast_signed() - base_bytes.cast_signed();
            println!(
                "{:<22}{:>12}{:>10}{:>12}{:>13.1}%  ({:+} B)",
                name,
                csr.uniq_lists,
                csr.list_ids.len(),
                csr.bytes(),
                100.0 * hit,
                delta
            );
        }
        println!();
    }
    Ok(())
}

/// P(subcell owner == `list[0]`) over owned subcells in border cells.
fn early_exit(grid: &grid::CellGrid, csr: &grid::Csr) -> f64 {
    let (mut hit, mut total) = (0u64, 0u64);
    // row-major zip: primary cell c ↔ tallies cell c
    for (&tag, tallies) in csr.primary.iter().zip(grid.tallies.iter()) {
        if tag & 0x8000 == 0 {
            continue;
        }
        let list_index = (tag & 0x7FFF) as usize;
        let first = csr.list_ids[csr.list_offsets[list_index] as usize];
        for &(zone, count) in tallies {
            total += u64::from(count);
            if zone == first {
                hit += u64::from(count);
            }
        }
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "hit ≤ total = subcell tally sum ≪ 2^53; probability"
    )]
    let probability = if total == 0 {
        0.0
    } else {
        hit as f64 / total as f64
    };
    probability
}
