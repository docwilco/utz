// Measurement backlog #1 (PLAN.md §15): dominant-first candidate-list ordering —
// how many extra unique lists / KB does it cost vs id-sorted interning, and how
// much PIP early-exit does each ordering actually buy?
//
// Orderings:
//   id-sorted          — baseline, maximal interning
//   area-desc          — global zone area descending; deterministic per set, so
//                        interning is preserved BY CONSTRUCTION (verified here)
//   cell-dominant-first — this cell's dominant zone first; best early-exit,
//                        breaks interning (the cost being measured)
//
// Early-exit quality = fraction of owned subcells (0.25° at 2°) inside border
// cells whose owner equals list[0] — i.e. P(first PIP hit) for area-uniform
// lookups landing in border cells.
//
// usage: cargo run --release -p utz-build --example dominant_cost [deg] [ds...]

use utz_build::grid::{self, Order};

fn main() -> anyhow::Result<()> {
    let deg: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let dss: Vec<String> = {
        let v: Vec<String> = std::env::args().skip(2).collect();
        if v.is_empty() { vec!["now".into(), "1970".into()] } else { v }
    };
    for ds in &dss {
        let feats = utz_build::load(ds)?;
        let areas = grid::feat_areas(&feats);
        let g = grid::build(&feats, deg, 8);
        let border = g.sets.iter().filter(|s| s.len() > 1).count();
        println!("{} @ {deg}°  ({} zones, {} border cells)", ds.to_uppercase(), feats.len(), border);
        println!("{:<22}{:>12}{:>10}{:>12}{:>14}", "ordering", "uniq lists", "ids", "CSR bytes", "P(hit@[0])");
        println!("{}", "-".repeat(70));

        let mut base_bytes = 0usize;
        for (name, order) in [("id-sorted", Order::IdSorted),
                              ("area-desc", Order::AreaDesc),
                              ("cell-dominant-first", Order::CellDominantFirst)] {
            let csr = grid::intern_csr(&g, order, &areas);
            let hit = early_exit(&g, &csr);
            if order == Order::IdSorted { base_bytes = csr.bytes(); }
            let delta = csr.bytes() as isize - base_bytes as isize;
            println!("{:<22}{:>12}{:>10}{:>12}{:>13.1}%  ({:+} B)",
                name, csr.uniq_lists, csr.list_ids.len(), csr.bytes(), 100.0 * hit, delta);
        }
        println!();
    }
    Ok(())
}

/// P(subcell owner == list[0]) over owned subcells in border cells.
fn early_exit(g: &grid::CellGrid, csr: &grid::Csr) -> f64 {
    let (mut hit, mut tot) = (0u64, 0u64);
    for c in 0..g.ncols * g.nrows {
        let p = csr.primary[c];
        if p & 0x8000 == 0 { continue; }
        let li = (p & 0x7FFF) as usize;
        let first = csr.list_ids[csr.list_offsets[li] as usize];
        for &(z, n) in &g.tallies[c] {
            tot += n as u64;
            if z == first { hit += n as u64; }
        }
    }
    if tot == 0 { 0.0 } else { hit as f64 / tot as f64 }
}
