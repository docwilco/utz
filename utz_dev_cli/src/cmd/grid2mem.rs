//! Computes the exact memory of a grid at one cell size: it measures
//! candidate (zone) counts per border cell, then sizes several layouts,
//! showing 32- vs 64-bit differences.
//!
//! ```text
//! utz_dev_cli grid2mem [ds] [deg]
//! ```
use std::collections::HashSet;

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The grid cell size in degrees.
    #[arg(default_value_t = 2.0)]
    deg: f64,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
///
/// # Panics
/// The command panics if the dataset has more features than fit a `u16`
/// id.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, deg) = (args.ds, args.deg);

    // rings tagged with their feature (zone) id
    let features = utz_build::load(&dataset)?;
    let rings: Vec<(u16, Vec<(f64, f64)>)> = features
        .iter()
        .enumerate()
        .flat_map(|(fid, feature)| {
            feature.polys.iter().flatten().map(move |ring| {
                (
                    u16::try_from(fid).expect("feature id fits u16"),
                    ring.clone(),
                )
            })
        })
        .collect();
    let n_features = rings.iter().map(|(fid, _)| *fid).max().unwrap_or(0) as usize + 1;

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ceil(360/deg) is a small positive integer"
    )]
    let ncols = (360.0 / deg).ceil() as usize;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ceil(180/deg) is a small positive integer"
    )]
    let nrows = (180.0 / deg).ceil() as usize;
    let total = ncols * nrows;
    let mut sets: Vec<HashSet<u16>> = vec![HashSet::new(); total];
    let cell = |lon: f64, lat: f64| -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_possible_wrap,
            reason = "cell index, fraction dropped then clamped"
        )]
        let col = (((lon + 180.0) / deg) as isize).clamp(0, ncols as isize - 1) as usize;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_possible_wrap,
            reason = "cell index, fraction dropped then clamped"
        )]
        let row = (((lat + 90.0) / deg) as isize).clamp(0, nrows as isize - 1) as usize;
        row * ncols + col
    };
    for (fid, ring) in &rings {
        let ring_len = ring.len();
        for i in 0..ring_len {
            let (x0, y0) = ring[i];
            let (x1, y1) = ring[(i + 1) % ring_len];
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "edge span in cells is small and non-negative"
            )]
            let steps =
                ((((x1 - x0).abs()).max((y1 - y0).abs()) / deg * 2.0).ceil() as usize).max(1);
            for step in 0..=steps {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "step ≤ steps = small per-edge cell span; exact"
                )]
                let t = step as f64 / steps as f64;
                sets[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)].insert(*fid);
            }
        }
    }

    let border: usize = sets.iter().filter(|set| set.len() > 1).count();
    let interior_or_empty = total - border;
    let multi_ids: usize = sets
        .iter()
        .filter(|set| set.len() > 1)
        .map(std::collections::HashSet::len)
        .sum();
    let max_candidates = sets
        .iter()
        .map(std::collections::HashSet::len)
        .max()
        .unwrap_or(0);

    println!(
        "{} @ {deg}deg  ({n_features} zones)",
        dataset.to_uppercase()
    );
    println!("  grid: {ncols} x {nrows} = {total} cells");
    println!("  border cells (>1 zone): {border}   single/empty: {interior_or_empty}");
    #[expect(
        clippy::cast_precision_loss,
        reason = "candidate-id and border-cell counts ≪ 2^53; avg display"
    )]
    let avg = multi_ids as f64 / border.max(1) as f64;
    println!(
        "  candidate ids in border cells: {multi_ids}  (avg {avg:.2}/border, max {max_candidates})\n"
    );

    // ---- layout A: flat CSR (fixed-width, platform-independent) ----
    // primary: u16 per cell (zone id, or spill index w/ high-bit flag)
    // offsets: u32 per border cell +1 ; ids: u16 per candidate entry
    let layout_a = total * 2 + (border + 1) * 4 + multi_ids * 2;
    // ---- layout B: primary u16 + inline blob (count u8 + ids), offset u32 ----
    let layout_b = total * 2 + (border + 1) * 4 + border + multi_ids * 2;
    // ---- layout C (naive): Vec<Vec<u16>> — platform dependent ----
    let vec_header32 = 12usize;
    let vec_header64 = 24usize;
    let alloc = 16usize; // rough per-allocation heap overhead
                         // every non-empty cell heap-allocates its inner Vec
    let nonempty = total - sets.iter().filter(|set| set.is_empty()).count();
    let all_ids: usize = sets.iter().map(std::collections::HashSet::len).sum();
    let layout_c32 = total * vec_header32 + nonempty * alloc + all_ids * 2;
    let layout_c64 = total * vec_header64 + nonempty * alloc + all_ids * 2;

    // ---- interned CSR: dedup identical candidate lists (coastlines repeat {land,ocean}) ----
    let mut unique: HashSet<Vec<u16>> = HashSet::new();
    for set in &sets {
        if set.len() > 1 {
            let mut list: Vec<u16> = set.iter().copied().collect();
            list.sort_unstable();
            unique.insert(list);
        }
    }
    let uniq_lists = unique.len();
    let uniq_ids: usize = unique.iter().map(std::vec::Vec::len).sum();
    // primary u16 + list_offsets u16[uniq+1] + list_ids u16[uniq_ids]
    let interned = total * 2 + (uniq_lists + 1) * 2 + uniq_ids * 2;

    #[expect(
        clippy::cast_precision_loss,
        reason = "layout byte estimates ≪ 2^53; KB display"
    )]
    let kb = |n: usize| format!("{:.1} KB", n as f64 / 1024.0);
    println!("  unique candidate lists among border cells: {uniq_lists}  ({uniq_ids} ids)");
    println!(
        "  layout D  interned CSR (u16 everywhere): {}   <- dedup repeated lists",
        kb(interned)
    );
    println!(
        "  layout A  flat CSR (u16/u32 arrays):     {}   (32-bit == 64-bit, fixed width)",
        kb(layout_a)
    );
    println!(
        "  layout B  flat + inline counts:          {}   (32-bit == 64-bit)",
        kb(layout_b)
    );
    println!(
        "  layout C  naive Vec<Vec<u16>>  32-bit:   {}",
        kb(layout_c32)
    );
    println!(
        "  layout C  naive Vec<Vec<u16>>  64-bit:   {}",
        kb(layout_c64)
    );
    Ok(())
}
