// Measurement backlog #2 (PLAN.md §15): hand-rolled i64 PIP vs the geo i64
// oracle vs geometry-rs (tzf-rs's PIP crate, tidwall/geometry port) on real
// OSM geometry — correctness (target 0 disagreements vs geo) + speed.
//
// All contenders get the SAME quantized (i24) simplified geometry and run the
// same linear first-hit scan with the same hoisted bbox precheck, so the
// comparison is pure per-edge PIP.
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
    // ring slices + bbox hoisted out of the loop (the runtime's grid plays
    // this role; every contender below gets the same hoisted bbox test)
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

    // ---- geo oracle, same scan order, same hoisted bbox precheck ----
    // (geo 0.32 Polygon::contains has NO internal bounding-rect precheck —
    // coordinate_position walks the exterior ring directly — so hoist the same
    // bbox test ours gets, keeping the comparison pure per-edge PIP)
    let t = Instant::now();
    let mut oracle: Vec<Option<usize>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        let pt = geo::Point::new(px as i64, py as i64);
        oracle.push(gpolys.iter().zip(&polys)
            .find(|((_, p), b)| px >= b.bbox.0 && py >= b.bbox.1 && px <= b.bbox.2 && py <= b.bbox.3
                && p.contains(&pt))
            .map(|(&(fi, _), _)| fi));
    }
    let t_geo = t.elapsed();

    // ---- geometry-rs, same scan order, same hoisted bbox precheck ----
    // (contains_point also runs its own internal rect precheck — inherent to
    // its API, tzf-rs pays it too, and it's noise next to the ring walk)
    let gm_polys: Vec<(usize, geometry_rs::Polygon)> = quant.iter().enumerate()
        .flat_map(|(fi, (_, polys))| polys.iter().map(move |p| {
            let ring = |r: &Vec<(i32, i32)>| -> Vec<geometry_rs::Point> {
                let mut v: Vec<geometry_rs::Point> = r.iter()
                    .map(|&(x, y)| geometry_rs::Point { x: x as f64, y: y as f64 })
                    .collect();
                let first = v[0];
                v.push(first); // expects closed GeoJSON-style rings
                v
            };
            (fi, geometry_rs::Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
        }))
        .collect();
    let t = Instant::now();
    let mut gm: Vec<Option<usize>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        let pt = geometry_rs::Point { x: px as f64, y: py as f64 };
        gm.push(gm_polys.iter().zip(&polys)
            .find(|((_, p), b)| px >= b.bbox.0 && py >= b.bbox.1 && px <= b.bbox.2 && py <= b.bbox.3
                && p.contains_point(pt))
            .map(|(&(fi, _), _)| fi));
    }
    let t_gm = t.elapsed();

    let mut diff = 0usize;
    for (i, (a, b)) in ours.iter().zip(&oracle).enumerate() {
        if a != b {
            diff += 1;
            let tz = |o: &Option<usize>| o.map(|f| quant[f].0.clone()).unwrap_or_default();
            println!("  disagree at pt#{i} {:?}: ours={:?} geo={:?}", pts[i], tz(a), tz(b));
        }
    }
    println!("disagreements vs geo: {diff}/{npts}");
    // geometry-rs boundary semantics differ (exterior edge = outside, hole
    // edge = inside), so only count — off-boundary answers must still agree
    let gm_diff = ours.iter().zip(&gm).filter(|(a, b)| a != b).count();
    println!("disagreements vs geometry-rs: {gm_diff}/{npts} (boundary semantics differ)");
    println!("ours:        {:>8.2?}  ({:.1} µs/lookup)", t_ours, t_ours.as_micros() as f64 / npts as f64);
    println!("geo:         {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_geo, t_geo.as_micros() as f64 / npts as f64,
        t_geo.as_secs_f64() / t_ours.as_secs_f64());
    println!("geometry-rs: {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_gm, t_gm.as_micros() as f64 / npts as f64,
        t_gm.as_secs_f64() / t_ours.as_secs_f64());
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
