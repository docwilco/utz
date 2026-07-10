// Measurement backlog #3 (PLAN.md §15): real grid lookup bench — interned-CSR
// grid prefilter (interior O(1), border cells → dominant-first PIP) vs the
// plain linear first-hit scan, on the same quantized simplified geometry.
//
// usage: utz-build grid-bench [ds] [eps_m] [deg] [npts]

use std::time::Instant;

use utz_build::grid::{self, Order};
use utz_build::{qx, qy, topo, Feat, QMAX};

struct QPoly {
    bbox: (i32, i32, i32, i32),
    rings: Vec<Vec<(i32, i32)>>,
}

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// grid cell size in degrees
    #[arg(default_value_t = 2.0)]
    deg: f64,
    /// number of sample points
    #[arg(default_value_t = 100_000)]
    npts: usize,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let (ds, eps_m, deg, npts) = (a.ds, a.eps_m, a.deg, a.npts);

    let feats = utz_build::load(&ds)?;
    let out = topo::encode_topology(&feats, eps_m / 111_320.0);
    let g = grid::build(&out.simplified, deg, 8);
    let areas = grid::feat_areas(&out.simplified);
    let csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);
    let fpolys: Vec<Vec<QPoly>> = out.simplified.iter().map(quantize).collect();
    println!("{} eps={eps_m}m grid={deg}°: {} features, {} uniq lists, {:.1} KB CSR, {npts} points",
        ds.to_uppercase(), fpolys.len(), csr.uniq_lists, csr.bytes() as f64 / 1024.0);

    let pts: Vec<(i32, i32)> = gen_pts(npts).iter().map(|&(lo, la)| (qx(lo), qy(la))).collect();
    let (ncols, nrows) = (g.ncols, g.nrows);
    let cell_of = |px: i32, py: i32| -> usize {
        let lon = f64::from(px) / QMAX * 180.0;
        let lat = f64::from(py) / QMAX * 90.0;
        let c = (((lon + 180.0) / deg) as isize).clamp(0, ncols as isize - 1) as usize;
        let r = (((lat + 90.0) / deg) as isize).clamp(0, nrows as isize - 1) as usize;
        r * ncols + c
    };
    let contains_feat = |fid: u16, px: i32, py: i32| -> bool {
        fpolys[fid as usize].iter().any(|p|
            px >= p.bbox.0 && py >= p.bbox.1 && px <= p.bbox.2 && py <= p.bbox.3 && {
                let rings: Vec<&[(i32, i32)]> = p.rings.iter().map(std::vec::Vec::as_slice).collect();
                utz::pip::contains_i64(&rings, px, py)
            })
    };

    // ---- grid lookup ----
    let (mut pip_needed, mut fallback) = (0usize, 0usize);
    let t = Instant::now();
    let mut got: Vec<Option<u16>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        let p = csr.primary[cell_of(px, py)];
        got.push(if p == 0x7FFF {
            None
        } else if p & 0x8000 == 0 {
            Some(p) // interior cell: O(1)
        } else {
            pip_needed += 1;
            let li = (p & 0x7FFF) as usize;
            let list = &csr.list_ids[csr.list_offsets[li] as usize..csr.list_offsets[li + 1] as usize];
            let hit = list.iter().copied().find(|&fid| contains_feat(fid, px, py));
            if hit.is_none() { fallback += 1; } // quantization pushed the point off every candidate
            Some(hit.unwrap_or(list[0]))
        });
    }
    let t_grid = t.elapsed();

    // ---- linear first-hit scan, same geometry ----
    let t = Instant::now();
    let mut lin: Vec<Option<u16>> = Vec::with_capacity(npts);
    for &(px, py) in &pts {
        lin.push((0..fpolys.len() as u16).find(|&fid| contains_feat(fid, px, py)));
    }
    let t_lin = t.elapsed();

    // agreement (tzid-level: dominant-first order vs id order may pick either
    // side of a shared border for boundary-claimed points)
    let tz = |o: &Option<u16>| o.map(|f| out.simplified[f as usize].tzid.clone().unwrap_or_default());
    // disagreements where both answers contain the point are benign (TZBB
    // overlap areas / boundary claiming — either tzid is valid); a grid answer
    // that does NOT contain the point is genuinely wrong.
    let (mut diff, mut wrong, mut shown) = (0usize, 0usize, 0usize);
    for (i, (a, b)) in got.iter().zip(&lin).enumerate() {
        if tz(a) == tz(b) { continue; }
        diff += 1;
        let (px, py) = pts[i];
        let ok = a.is_some_and(|fa| contains_feat(fa, px, py));
        if !ok {
            wrong += 1;
            if shown < 8 {
                shown += 1;
                let (lon, lat) = (f64::from(px) / QMAX * 180.0, f64::from(py) / QMAX * 90.0);
                let p = csr.primary[cell_of(px, py)];
                println!("  WRONG ({lon:.4},{lat:.4}) grid={:?} lin={:?} cell={}",
                    tz(a), tz(b), if p & 0x8000 != 0 { "border" } else { "interior" });
            }
        }
    }
    println!("  disagreements: {diff} ({wrong} wrong, {} benign-overlap)", diff - wrong);

    println!("  PIP needed: {pip_needed}/{npts} ({:.1}%)   fallbacks: {fallback}   tzid disagreements vs linear: {diff}",
        100.0 * pip_needed as f64 / npts as f64);
    println!("  grid:   {:>9.2?}  ({:.2} µs/lookup)", t_grid, t_grid.as_micros() as f64 / npts as f64);
    println!("  linear: {:>9.2?}  ({:.2} µs/lookup)   grid speedup {:.1}x\n",
        t_lin, t_lin.as_micros() as f64 / npts as f64,
        t_lin.as_secs_f64() / t_grid.as_secs_f64());
    Ok(())
}

fn quantize(f: &Feat) -> Vec<QPoly> {
    f.polys.iter().filter_map(|p| {
        let rings: Vec<Vec<(i32, i32)>> = p.iter().map(|r| {
            let mut q: Vec<(i32, i32)> = r.iter().map(|&(x, y)| (qx(x), qy(y))).collect();
            q.dedup();
            if q.first() == q.last() && q.len() > 1 { q.pop(); }
            q
        }).filter(|r| r.len() >= 3).collect();
        if rings.is_empty() { return None; }
        let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y) in &rings[0] {
            bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
        }
        Some(QPoly { bbox: bb, rings })
    }).collect()
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || { lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}
