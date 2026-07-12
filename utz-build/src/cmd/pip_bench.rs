// Measurement backlog #2 (PLAN.md §15): hand-rolled i64 PIP vs the geo i64
// oracle vs geometry-rs (tzf-rs's PIP crate, tidwall/geometry port) on real
// OSM geometry — correctness (target 0 disagreements vs geo) + speed.
//
// All contenders get the SAME quantized (i24) simplified geometry and run the
// same linear first-hit scan with the same hoisted bbox precheck, so the
// comparison is pure per-edge PIP.
//
// usage: utz-build pip-bench [ds] [eps_m] [npts]

use std::time::Instant;

use geo::Contains;

use utz_build::{q24_lat, q24_lon, topo, Feat};

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// number of sample points
    #[arg(default_value_t = 20_000)]
    npts: usize,
}

/// Per-polygon contender record: ring slices + bbox hoisted out of the loop
/// (the runtime's grid plays this role; every contender gets the same
/// hoisted bbox test).
struct P<'a> { fi: usize, bbox: (i32, i32, i32, i32), rings: Vec<&'a [(i32, i32)]> }

pub fn run(a: Args) -> utz_build::Result<()> {
    let (ds, eps_m, npts) = (a.ds, a.eps_m, a.npts);

    let feats = utz_build::load(&ds)?;
    let out = topo::encode_topology(&feats, eps_m / 111_320.0);
    let quant = quantize(&out.simplified);
    let verts: usize = quant.iter().flat_map(|(_, ps)| ps).flatten().map(std::vec::Vec::len).sum();
    println!("{} eps={eps_m}m: {} features, {verts} quantized verts, {npts} points\n",
        ds.to_uppercase(), quant.len());

    // geo oracle polygons over the identical i64 coords
    let gpolys: Vec<(usize, geo::Polygon<i64>)> = quant.iter().enumerate()
        .flat_map(|(fi, (_, polys))| polys.iter().map(move |p| {
            let ring = |r: &Vec<(i32, i32)>| -> geo::LineString<i64> {
                let mut v: Vec<(i64, i64)> = r.iter().map(|&(x, y)| (i64::from(x), i64::from(y))).collect();
                if v.first() != v.last() { if let Some(&f) = v.first() { v.push(f); } }
                v.into()
            };
            (fi, geo::Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
        }))
        .collect();

    let pts: Vec<(i32, i32)> = gen_pts(npts).iter().map(|&(lo, la)| (q24_lon(lo), q24_lat(la))).collect();

    // ---- ours: per-polygon integer PIP, linear first-hit scan ----
    let polys: Vec<P> = quant.iter().enumerate()
        .flat_map(|(fi, (_, ps))| ps.iter().map(move |p| {
            let rings: Vec<&[(i32, i32)]> = p.iter().map(std::vec::Vec::as_slice).collect();
            let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &(x, y) in &p[0] {
                bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
            }
            P { fi, bbox: bb, rings }
        }))
        .collect();
    // geometry-rs polygons (expects closed GeoJSON-style rings)
    let gm_polys: Vec<(usize, geometry_rs::Polygon)> = quant.iter().enumerate()
        .flat_map(|(fi, (_, polys))| polys.iter().map(move |p| {
            let ring = |r: &Vec<(i32, i32)>| -> Vec<geometry_rs::Point> {
                let mut v: Vec<geometry_rs::Point> = r.iter()
                    .map(|&(x, y)| geometry_rs::Point { x: f64::from(x), y: f64::from(y) })
                    .collect();
                let first = v[0];
                v.push(first);
                v
            };
            (fi, geometry_rs::Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
        }))
        .collect();

    // untimed warmup: first touch of `polys`/`pts` — without it the first
    // timed contender eats every cold miss and the shared-structure rows
    // (f64/i128) look spuriously fast
    let (warm, _) = timed_scan(&pts, &polys, |i, px, py| utz::pip::contains_i64(&polys[i].rings, px, py));
    assert!(warm.iter().flatten().count() > 0);

    // ---- the contenders, all through the same scan shell ----
    // ours-f64: same kernel shape, f64 arithmetic (test/bench-only
    // instantiation, bit-exact at this i24 quant — pip.rs module docs).
    // ours-i128: the i32-quant width on i24 data (wider is always exact) —
    // what an i32-quant asset would pay on this host.
    // geo 0.32 Polygon::contains has NO internal bounding-rect precheck
    // (coordinate_position walks the exterior ring directly), so the shared
    // hoisted bbox test keeps the comparison pure per-edge PIP.
    // geometry-rs contains_point runs its own internal rect precheck on top —
    // inherent to its API, tzf-rs pays it too, noise next to the ring walk.
    let (ours, t_ours) =
        timed_scan(&pts, &polys, |i, px, py| utz::pip::contains_i64(&polys[i].rings, px, py));
    let (ours_f64, t_f64) =
        timed_scan(&pts, &polys, |i, px, py| utz::pip::contains_f64(&polys[i].rings, px, py));
    let (ours_i128, t_i128) =
        timed_scan(&pts, &polys, |i, px, py| utz::pip::contains_i128(&polys[i].rings, px, py));
    let (oracle, t_geo) = timed_scan(&pts, &polys, |i, px, py| {
        gpolys[i].1.contains(&geo::Point::new(i64::from(px), i64::from(py)))
    });
    let (gm, t_gm) = timed_scan(&pts, &polys, |i, px, py| {
        gm_polys[i].1.contains_point(geometry_rs::Point { x: f64::from(px), y: f64::from(py) })
    });

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
    let f64_diff = ours.iter().zip(&ours_f64).filter(|(a, b)| a != b).count();
    println!("disagreements vs ours-f64: {f64_diff}/{npts} (must be 0: f64 exact at i24)");
    let i128_diff = ours.iter().zip(&ours_i128).filter(|(a, b)| a != b).count();
    println!("disagreements vs ours-i128: {i128_diff}/{npts} (must be 0: wider is exact)");
    #[expect(clippy::cast_precision_loss, reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/lookup display")]
    let us = |t: std::time::Duration| t.as_micros() as f64 / npts as f64;
    println!("ours:        {:>8.2?}  ({:.1} µs/lookup)", t_ours, us(t_ours));
    println!("ours-f64:    {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_f64, us(t_f64),
        t_f64.as_secs_f64() / t_ours.as_secs_f64());
    println!("ours-i128:   {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_i128, us(t_i128),
        t_i128.as_secs_f64() / t_ours.as_secs_f64());
    println!("geo:         {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_geo, us(t_geo),
        t_geo.as_secs_f64() / t_ours.as_secs_f64());
    println!("geometry-rs: {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_gm, us(t_gm),
        t_gm.as_secs_f64() / t_ours.as_secs_f64());
    Ok(())
}


/// Linear first-hit scan of every contender poly with the shared hoisted
/// bbox precheck — the scan shell every contender pays identically; `hit`
/// is the per-candidate containment kernel. Returns per-point first hit
/// (as feature index) + wall time.
fn timed_scan(
    pts: &[(i32, i32)],
    polys: &[P],
    hit: impl Fn(usize, i32, i32) -> bool,
) -> (Vec<Option<usize>>, std::time::Duration) {
    let t = Instant::now();
    let got = pts.iter().map(|&(px, py)| {
        (0..polys.len())
            .find(|&i| {
                let b = &polys[i];
                px >= b.bbox.0 && py >= b.bbox.1 && px <= b.bbox.2 && py <= b.bbox.3
                    && hit(i, px, py)
            })
            .map(|i| polys[i].fi)
    }).collect();
    (got, t.elapsed())
}

/// tzid + polygon → ring → quantized vertices
type QFeat = (String, Vec<Vec<Vec<(i32, i32)>>>);

/// tzid + per-polygon rings quantized to the i24 grid, degenerate rings dropped.
fn quantize(feats: &[Feat]) -> Vec<QFeat> {
    feats.iter().map(|f| {
        let polys = f.polys.iter().map(|p| {
            p.iter().map(|r| {
                let mut q: Vec<(i32, i32)> = r.iter().map(|&(x, y)| (q24_lon(x), q24_lat(y))).collect();
                q.dedup();
                if q.first() == q.last() && q.len() > 1 { q.pop(); }
                q
            }).filter(|r| r.len() >= 3).collect::<Vec<_>>()
        }).filter(|p: &Vec<Vec<(i32, i32)>>| !p.is_empty()).collect();
        (f.tzid.clone().unwrap_or_default(), polys)
    }).collect()
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
