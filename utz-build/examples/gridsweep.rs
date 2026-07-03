// Grid-size sweep (1..=20 deg). For each size: total cells, "border" cells (a tz
// boundary edge passes through -> lookup needs PIP), interior cells (single zone ->
// O(1)), the fraction of area-uniform lookups that hit a border cell, and a memory
// estimate. usage: cargo run --release --example gridsweep <ned|osm|osm1970>
use std::io::BufReader;
use flatgeobuf::{FallibleStreamingIterator, FgbReader};
use geo_types::Geometry;
use geozero::ToGeo;

fn main() -> anyhow::Result<()> {
    let ds = std::env::args().nth(1).unwrap_or_else(|| "osm".into());
    let path = format!("/home/drwilco/spatialtime/assets/timezones_{ds}.fgb");
    // load all edges as (lon0,lat0,lon1,lat1)
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
    {
        let bytes = std::fs::read(&path)?;
        let mut r = BufReader::new(&bytes[..]);
        let fgb = FgbReader::open(&mut r)?;
        let mut seq = fgb.select_all_seq()?;
        while let Some(f) = seq.next()? {
            if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
                for p in &mp {
                    rings.push(p.exterior().coords().map(|c| (c.x, c.y)).collect());
                    for h in p.interiors() { rings.push(h.coords().map(|c| (c.x, c.y)).collect()); }
                }
            }
        }
    }
    let nedges: usize = rings.iter().map(|r| r.len()).sum();
    println!("{}: {} rings, ~{} edges\n", ds.to_uppercase(), rings.len(), nedges);
    println!("{:>4}{:>12}{:>12}{:>11}{:>13}{:>11}", "deg", "cells", "border", "interior", "P(PIP)", "grid mem");
    println!("{}", "-".repeat(63));

    for d in 1u32..=20 {
        let df = d as f64;
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
