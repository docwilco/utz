// Grid-size sweep (1..=20 deg). For each size: total cells, "border" cells (a tz
// boundary edge passes through -> lookup needs PIP), interior cells (single zone ->
// O(1)), the fraction of area-uniform lookups that hit a border cell, and a memory
// estimate. usage: utz-build gridsweep [ds]

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let ds = a.ds;
    let feats = utz_build::load(&ds)?;
    let rings: Vec<Vec<(f64, f64)>> = feats.iter()
        .flat_map(|f| f.polys.iter().flatten().cloned())
        .collect();
    let nedges: usize = rings.iter().map(std::vec::Vec::len).sum();
    println!("{}: {} rings, ~{} edges\n", ds.to_uppercase(), rings.len(), nedges);
    println!("{:>4}{:>12}{:>12}{:>11}{:>13}{:>11}", "deg", "cells", "border", "interior", "P(PIP)", "grid mem");
    println!("{}", "-".repeat(63));

    for d in 1u32..=20 {
        let df = f64::from(d);
        let ncols = ((360.0 / df).ceil()) as usize;
        let nrows = ((180.0 / df).ceil()) as usize;
        let total = ncols * nrows;
        let mut border = vec![false; total];
        let cell = |lon: f64, lat: f64| -> usize {
            let c = (((lon + 180.0) / df) as isize).clamp(0, ncols as isize - 1) as usize;
            let r = (((lat + 90.0) / df) as isize).clamp(0, nrows as isize - 1) as usize;
            r * ncols + c
        };
        for ring in &rings {
            let n = ring.len();
            for i in 0..n {
                let (x0, y0) = ring[i];
                let (x1, y1) = ring[(i + 1) % n];
                let span = (((x1 - x0).abs()).max((y1 - y0).abs()) / df * 2.0).ceil() as usize;
                let steps = span.max(1);
                for s in 0..=steps {
                    let t = s as f64 / steps as f64;
                    border[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)] = true;
                }
            }
        }
        let b = border.iter().filter(|&&x| x).count();
        let interior = total - b;
        let p_pip = 100.0 * b as f64 / total as f64;
        // dense primary-zone u16 per cell + ~2 spillover u16 per border cell
        let mem = total * 2 + b * 4;
        println!("{:>4}{:>12}{:>12}{:>11}{:>12.1}%{:>9} KB", d, total, b, interior, p_pip, mem / 1024);
    }
    Ok(())
}
