//! Real grid lookup bench: interned-CSR grid prefilter (interior O(1),
//! border cells → dominant-first PIP) vs the plain linear first-hit scan,
//! on the same quantized simplified geometry.
//!
//! ```text
//! utz_dev_cli grid-bench [ds] [eps_m] [deg] [npts]
//! ```

use std::time::Instant;

use utz_encode::grid::{self, Order};
use utz_encode::{q24_lat, q24_lon, topo, Feat, QMAX_I24};

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

/// # Errors
/// Dataset load/parse failure.
///
/// # Panics
/// If the dataset has more features than fit a `u16` id.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, eps_m, deg, n_points) = (args.ds, args.eps_m, args.deg, args.npts);

    let features = utz_build::load(&dataset)?;
    let out = topo::encode_topology(&features, eps_m / 111_320.0);
    let grid = grid::build(&out.simplified, deg, 8);
    let areas = grid::feat_areas(&out.simplified);
    let csr = grid::intern_csr(&grid, Order::CellDominantFirst, &areas);
    let feature_polys: Vec<Vec<QPoly>> = out.simplified.iter().map(quantize).collect();
    #[expect(
        clippy::cast_precision_loss,
        reason = "CSR byte size ≪ 2^53; KB display"
    )]
    let csr_kb = csr.bytes() as f64 / 1024.0;
    println!("{} eps={eps_m}m grid={deg}°: {} features, {} uniq lists, {csr_kb:.1} KB CSR, {n_points} points",
        dataset.to_uppercase(), feature_polys.len(), csr.uniq_lists);

    let points: Vec<(i32, i32)> = gen_pts(n_points)
        .iter()
        .map(|&(lon, lat)| (q24_lon(lon), q24_lat(lat)))
        .collect();
    let (ncols, nrows) = (grid.ncols(), grid.nrows());
    let cell_of = |px: i32, py: i32| -> usize {
        let lon = f64::from(px) / QMAX_I24 * 180.0;
        let lat = f64::from(py) / QMAX_I24 * 90.0;
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
    let contains_feat = |fid: u16, px: i32, py: i32| -> bool {
        feature_polys[fid as usize].iter().any(|poly| {
            px >= poly.bbox.0 && py >= poly.bbox.1 && px <= poly.bbox.2 && py <= poly.bbox.3 && {
                let rings: Vec<&[(i32, i32)]> =
                    poly.rings.iter().map(std::vec::Vec::as_slice).collect();
                utz::pip::contains::<i64, _>(&rings, px, py)
            }
        })
    };

    // ---- grid lookup ----
    let (mut pip_needed, mut fallback) = (0usize, 0usize);
    let timer = Instant::now();
    let mut got: Vec<Option<u16>> = Vec::with_capacity(n_points);
    for &(px, py) in &points {
        let tag = csr.primary[cell_of(px, py)];
        got.push(if tag == 0x7FFF {
            None
        } else if tag & 0x8000 == 0 {
            Some(tag) // interior cell: O(1)
        } else {
            pip_needed += 1;
            let list_index = (tag & 0x7FFF) as usize;
            let list = &csr.list_ids
                [csr.list_offsets[list_index] as usize..csr.list_offsets[list_index + 1] as usize];
            let hit = list.iter().copied().find(|&fid| contains_feat(fid, px, py));
            if hit.is_none() {
                fallback += 1;
            } // quantization pushed the point off every candidate
            Some(hit.unwrap_or(list[0]))
        });
    }
    let t_grid = timer.elapsed();

    // ---- linear first-hit scan, same geometry ----
    let timer = Instant::now();
    let mut linear: Vec<Option<u16>> = Vec::with_capacity(n_points);
    for &(px, py) in &points {
        linear.push(
            (0..u16::try_from(feature_polys.len()).expect("feature count fits u16"))
                .find(|&fid| contains_feat(fid, px, py)),
        );
    }
    let t_linear = timer.elapsed();

    // agreement (tzid-level: dominant-first order vs id order may pick either
    // side of a shared border for boundary-claimed points)
    let tzid_of = |id: &Option<u16>| {
        id.map(|fid| {
            out.simplified[fid as usize]
                .tzid
                .clone()
                .unwrap_or_default()
        })
    };
    // disagreements where both answers contain the point are benign (TZBB
    // overlap areas / boundary claiming — either tzid is valid); a grid answer
    // that does NOT contain the point is genuinely wrong.
    let (mut diff, mut wrong, mut shown) = (0usize, 0usize, 0usize);
    for (i, (grid_answer, linear_answer)) in got.iter().zip(&linear).enumerate() {
        if tzid_of(grid_answer) == tzid_of(linear_answer) {
            continue;
        }
        diff += 1;
        let (px, py) = points[i];
        let ok = grid_answer.is_some_and(|fid| contains_feat(fid, px, py));
        if !ok {
            wrong += 1;
            if shown < 8 {
                shown += 1;
                let (lon, lat) = (
                    f64::from(px) / QMAX_I24 * 180.0,
                    f64::from(py) / QMAX_I24 * 90.0,
                );
                let tag = csr.primary[cell_of(px, py)];
                println!(
                    "  WRONG ({lon:.4},{lat:.4}) grid={:?} lin={:?} cell={}",
                    tzid_of(grid_answer),
                    tzid_of(linear_answer),
                    if tag & 0x8000 != 0 {
                        "border"
                    } else {
                        "interior"
                    }
                );
            }
        }
    }
    println!(
        "  disagreements: {diff} ({wrong} wrong, {} benign-overlap)",
        diff - wrong
    );

    #[expect(
        clippy::cast_precision_loss,
        reason = "pip_needed ≤ n_points point count ≪ 2^53; percentage display"
    )]
    let pip_pct = 100.0 * pip_needed as f64 / n_points as f64;
    println!("  PIP needed: {pip_needed}/{n_points} ({pip_pct:.1}%)   fallbacks: {fallback}   tzid disagreements vs linear: {diff}");
    #[expect(
        clippy::cast_precision_loss,
        reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/lookup display"
    )]
    let us = |elapsed: std::time::Duration| elapsed.as_micros() as f64 / n_points as f64;
    println!("  grid:   {:>9.2?}  ({:.2} µs/lookup)", t_grid, us(t_grid));
    println!(
        "  linear: {:>9.2?}  ({:.2} µs/lookup)   grid speedup {:.1}x\n",
        t_linear,
        us(t_linear),
        t_linear.as_secs_f64() / t_grid.as_secs_f64()
    );
    Ok(())
}

fn quantize(feature: &Feat) -> Vec<QPoly> {
    feature
        .polys
        .iter()
        .filter_map(|poly| {
            let rings: Vec<Vec<(i32, i32)>> = poly
                .iter()
                .map(|ring| {
                    let mut quantized: Vec<(i32, i32)> = ring
                        .iter()
                        .map(|&(x, y)| (q24_lon(x), q24_lat(y)))
                        .collect();
                    quantized.dedup();
                    if quantized.first() == quantized.last() && quantized.len() > 1 {
                        quantized.pop();
                    }
                    quantized
                })
                .filter(|ring| ring.len() >= 3)
                .collect();
            if rings.is_empty() {
                return None;
            }
            let mut bbox = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &(x, y) in &rings[0] {
                bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            }
            Some(QPoly { bbox, rings })
        })
        .collect()
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
