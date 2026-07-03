// Measurement backlog #2 (PLAN.md §15): hand-rolled i64 PIP vs the geo i64
// oracle on real OSM geometry — correctness (target 0 disagreements) + speed.
//
// Both sides get the SAME quantized (i24) simplified geometry and run the same
// linear first-hit scan over all polygons, so the comparison is pure PIP.
//
// usage: cargo run --release -p utz-build --example pip_bench [now|1970] [eps_m] [npts]

use std::time::Instant;

use geo::Contains;

use utz_build::{qx, qy, topo, Feat};

fn main() -> anyhow::Result<()> {
    let ds = std::env::args().nth(1).unwrap_or_else(|| "now".into());
    let eps_m: f64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(500.0);
    let npts: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(20_000);

    let feats = utz_build::load(&ds)?;
    let out = topo::encode_topology(&feats, eps_m / 111_320.0);
    let quant = quantize(&out.simplified);
    let verts: usize = quant.iter().flat_map(|(_, ps)| ps).flatten().map(|r| r.len()).sum();
    println!("{} eps={eps_m}m: {} features, {verts} quantized verts, {npts} points\n",
        ds.to_uppercase(), quant.len());

    // geo oracle polygons over the identical i64 coords
    let gpolys: Vec<(usize, geo::Polygon<i64>)> = quant.iter().enumerate()
        .flat_map(|(fi, (_, polys))| polys.iter().map(move |p| {
            let ring = |r: &Vec<(i32, i32)>| -> geo::LineString<i64> {
                let mut v: Vec<(i64, i64)> = r.iter().map(|&(x, y)| (x as i64, y as i64)).collect();
                if v.first() != v.last() { if let Some(&f) = v.first() { v.push(f); } }
                v.into()
            };
            (fi, geo::Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
        }))
        .collect();

    let pts: Vec<(i32, i32)> = gen_pts(npts).iter().map(|&(lo, la)| (qx(lo), qy(la))).collect();

    // ---- ours: per-polygon integer PIP, linear first-hit scan ----
    // ring slices + bbox hoisted out of the loop (geo's Contains has the same
    // bounding-rect precheck internally; the runtime's grid plays this role)
    struct P<'a> { fi: usize, bbox: (i32, i32, i32, i32), rings: Vec<&'a [(i32, i32)]> }
    let polys: Vec<P> = quant.iter().enumerate()
        .flat_map(|(fi, (_, ps))| ps.iter().map(move |p| {
            let rings: Vec<&[(i32, i32)]> = p.iter().map(|r| r.as_slice()).collect();
            let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &(x, y) in &p[0] {
                bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
            }
            P { fi, bbox: bb, rings }
        }))
        .collect();
    let t = Instant::now();
    let mut ours: Vec<Option<usize>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        ours.push(polys.iter()
            .find(|p| px >= p.bbox.0 && py >= p.bbox.1 && px <= p.bbox.2 && py <= p.bbox.3
                && utz::pip::contains_i64(&p.rings, px, py))
            .map(|p| p.fi));
    }
    let t_ours = t.elapsed();

    // ---- geo oracle, same scan order ----
    let t = Instant::now();
    let mut oracle: Vec<Option<usize>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        let pt = geo::Point::new(px as i64, py as i64);
        oracle.push(gpolys.iter().find(|(_, p)| p.contains(&pt)).map(|&(fi, _)| fi));
    }
    let t_geo = t.elapsed();

    let mut diff = 0usize;
    for (i, (a, b)) in ours.iter().zip(&oracle).enumerate() {
        if a != b {
            diff += 1;
            let tz = |o: &Option<usize>| o.map(|f| quant[f].0.clone()).unwrap_or_default();
            println!("  disagree at pt#{i} {:?}: ours={:?} geo={:?}", pts[i], tz(a), tz(b));
        }
    }
    println!("disagreements: {diff}/{npts}");
    println!("ours: {:>8.2?}  ({:.1} µs/lookup)", t_ours, t_ours.as_micros() as f64 / npts as f64);
    println!("geo:  {:>8.2?}  ({:.1} µs/lookup)   speedup {:.2}x",
        t_geo, t_geo.as_micros() as f64 / npts as f64,
        t_geo.as_secs_f64() / t_ours.as_secs_f64());
    Ok(())
}

/// tzid + per-polygon rings quantized to the i24 grid, degenerate rings dropped.
fn quantize(feats: &[Feat]) -> Vec<(String, Vec<Vec<Vec<(i32, i32)>>>)> {
    feats.iter().map(|f| {
        let polys = f.polys.iter().map(|p| {
            p.iter().map(|r| {
                let mut q: Vec<(i32, i32)> = r.iter().map(|&(x, y)| (qx(x), qy(y))).collect();
                q.dedup();
                if q.first() == q.last() && q.len() > 1 { q.pop(); }
                q
            }).filter(|r| r.len() >= 3).collect::<Vec<_>>()
        }).filter(|p: &Vec<Vec<(i32, i32)>>| !p.is_empty()).collect();
        (f.tzid.clone().unwrap_or_default(), polys)
    }).collect()
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || { lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}
