// Lookup benchmark: fgb R-tree (current spatialtime) vs fgb full-scan (no index)
// vs custom in-memory (linear bbox prefilter + integer PIP). Also reports memory.
// usage: cargo run --release --example bench <fgb-path>
use std::io::{BufReader, Cursor};
use std::time::Instant;
use flatgeobuf::{FallibleStreamingIterator, FeatureProperties, FgbReader};
use geo::{Intersects, Point};
use geo_types::Geometry;
use geozero::ToGeo;

struct CFeat { tzid: String, bbox: [i32; 4], rings: Vec<Vec<(i32, i32)>> }
const S: f64 = 1e7;
fn q(v: f64) -> i32 { (v * S).round() as i32 }

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/home/drwilco/spatialtime/assets/timezones_osm.fgb".into());
    let bytes = std::fs::read(&path)?;
    let file_mib = bytes.len() as f64 / (1 << 20) as f64;

    // ---- parse into custom in-memory form + stats ----
    let mut cf: Vec<CFeat> = Vec::new();
    let (mut nverts, mut biggest) = (0u64, 0usize);
    {
        let mut reader = BufReader::new(&bytes[..]);
        let fgb = FgbReader::open(&mut reader)?;
        let mut seq = fgb.select_all_seq()?;
        while let Some(f) = seq.next()? {
            let tzid = f.properties()?.get("tzid").cloned().unwrap_or_default();
            if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
                let (mut nx, mut ny, mut xx, mut xy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                let mut rings = Vec::new();
                let mut fv = 0usize;
                for p in &mp {
                    let mut allrings = vec![p.exterior().clone()];
                    allrings.extend(p.interiors().iter().cloned());
                    for r in &allrings {
                        let ring: Vec<(i32, i32)> = r.coords().map(|c| { let (a, b) = (q(c.x), q(c.y));
                            nx = nx.min(a); ny = ny.min(b); xx = xx.max(a); xy = xy.max(b); (a, b) }).collect();
                        fv += ring.len();
                        rings.push(ring);
                    }
                }
                nverts += fv as u64; biggest = biggest.max(fv);
                cf.push(CFeat { tzid, bbox: [nx, ny, xx, xy], rings });
            }
        }
    }
    println!("{}", path);
    println!("  features={}  verts={}  largest-feature-verts={}", cf.len(), nverts, biggest);
    println!("  fgb in RAM (decompressed): {:.1} MiB", file_mib);
    println!("  custom in-memory (i32 rings+bbox): {:.1} MiB", (nverts as f64 * 8.0 + cf.len() as f64 * 64.0) / (1 << 20) as f64);
    println!("  largest to_geo() transient (f64 geo::MultiPolygon): {:.1} MiB\n", biggest as f64 * 16.0 / (1 << 20) as f64);

    let pts = gen_pts(5000);

    // ---- custom lookup ----
    let cust = |lon: f64, lat: f64| -> String {
        let (px, py) = (q(lon), q(lat));
        for f in &cf {
            if px < f.bbox[0] || px > f.bbox[2] || py < f.bbox[1] || py > f.bbox[3] { continue; }
            let mut inside = false;
            for ring in &f.rings {
                let n = ring.len();
                for i in 0..n {
                    let (ax, ay) = ring[i]; let (bx, by) = ring[(i + 1) % n];
                    if (ay > py) != (by > py) {
                        let dy = (by - ay) as i64;
                        let lhs = (px - ax) as i64 * dy; let rhs = (bx - ax) as i64 * (py - ay) as i64;
                        if (dy > 0 && lhs < rhs) || (dy < 0 && lhs > rhs) { inside = !inside; }
                    }
                }
            }
            if inside { return f.tzid.clone(); }
        }
        String::new()
    };

    // ---- fgb R-tree lookup (mirrors spatialtime get_intersection) ----
    let rtree = |lon: f64, lat: f64| -> String {
        let mut reader = BufReader::new(&bytes[..]);
        let fgb = FgbReader::open(&mut reader).unwrap();
        let mut seq = fgb.select_bbox_seq(lon, lat, lon, lat).unwrap();
        while let Some(f) = seq.next().unwrap() {
            if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
                if mp.intersects(&Point::new(lon, lat)) {
                    return f.properties().unwrap().get("tzid").cloned().unwrap_or_default();
                }
            }
        }
        String::new()
    };

    // ---- fgb R-tree lookup, SEEKABLE (Cursor + select_bbox) — avoids streaming ----
    let rtree_seek = |lon: f64, lat: f64| -> String {
        let mut cur = Cursor::new(&bytes[..]);
        let fgb = FgbReader::open(&mut cur).unwrap();
        let mut seq = fgb.select_bbox(lon, lat, lon, lat).unwrap();
        while let Some(f) = seq.next().unwrap() {
            if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
                if mp.intersects(&Point::new(lon, lat)) {
                    return f.properties().unwrap().get("tzid").cloned().unwrap_or_default();
                }
            }
        }
        String::new()
    };

    // ---- fgb full-scan lookup (no index) ----
    let scan = |lon: f64, lat: f64| -> String {
        let mut reader = BufReader::new(&bytes[..]);
        let fgb = FgbReader::open(&mut reader).unwrap();
        let mut seq = fgb.select_all_seq().unwrap();
        while let Some(f) = seq.next().unwrap() {
            if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
                if mp.intersects(&Point::new(lon, lat)) {
                    return f.properties().unwrap().get("tzid").cloned().unwrap_or_default();
                }
            }
        }
        String::new()
    };

    // agreement sanity on first 500
    let mut disagree = 0;
    for &(lo, la) in pts.iter().take(500) { if cust(lo, la) != rtree(lo, la) { disagree += 1; } }
    println!("  custom vs R-tree disagreements (500 pts): {disagree}\n");

    let bench = |name: &str, n: usize, f: &dyn Fn(f64, f64) -> String| {
        let t = Instant::now();
        let mut hits = 0u64;
        for &(lo, la) in pts.iter().take(n) { if !f(lo, la).is_empty() { hits += 1; } }
        let us = t.elapsed().as_secs_f64() * 1e6 / n as f64;
        println!("  {:<22} {:>9.1} us/lookup   {:>10.0} lookups/s   ({n} pts, {hits} land)", name, us, 1e6 / us);
    };
    bench("fgb R-tree seq (current)", 5000, &rtree);
    bench("fgb R-tree seekable", 5000, &rtree_seek);
    bench("custom (bbox+intPIP)", 5000, &cust);
    bench("fgb full-scan (noidx)", 200, &scan);

    // isolate the fgb per-lookup FIXED overhead: open reader + rebuild R-tree +
    // read candidate headers, but DO NOT decode geometry (no to_geo).
    {
        let t = Instant::now();
        let mut cand = 0u64;
        for &(lo, la) in pts.iter().take(5000) {
            let mut reader = BufReader::new(&bytes[..]);
            let fgb = FgbReader::open(&mut reader).unwrap();
            let mut seq = fgb.select_bbox_seq(lo, la, lo, la).unwrap();
            while seq.next().unwrap().is_some() { cand += 1; }
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / 5000.0;
        println!("  {:<22} {:>9.1} us/lookup   (open+Rtree, NO decode; avg {} candidates)", "fgb overhead only", us, cand / 5000);
    }
    Ok(())
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x9e3779b97f4a7c15u64;
    let mut next = || { lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}
