//! Benchmarks the hand-rolled i64 PIP against the geo i64 oracle and
//! geometry-rs (tzf-rs's PIP crate, a tidwall/geometry port) on real OSM
//! geometry, measuring correctness (the target is 0 disagreements vs geo)
//! and speed.
//!
//! All contenders get the *same* quantized (i24) simplified geometry and run the
//! same linear first-hit scan with the same hoisted bbox precheck, so the
//! comparison is pure per-edge PIP.
//!
//! ```text
//! utz_dev_cli pip-bench [ds] [epsilon_m] [npts]
//! ```

use std::time::Instant;

use geo::Contains;

use utz_encode::{q24_lat, q24_lon, topo, Feat};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    epsilon_m: f64,
    /// The number of sample points.
    #[arg(default_value_t = 20_000)]
    npts: usize,
}

/// One per-polygon contender record, holding the ring slices and the bbox
/// hoisted out of the loop (the runtime's grid plays this role; every
/// contender gets the same hoisted bbox test).
struct P<'a> {
    fi: usize,
    bbox: (i32, i32, i32, i32),
    rings: Vec<&'a [(i32, i32)]>,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
///
/// # Panics
/// The command panics if the warmup scan lands no sample point inside any
/// zone (a sanity check on the quantized geometry).
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, epsilon_m, n_points) = (args.ds, args.epsilon_m, args.npts);

    let features = utz_build::load(&dataset)?;
    let out = topo::encode_topology(&features, epsilon_m / utz_common::METERS_PER_DEG);
    let quantized = quantize(&out.simplified);
    let verts: usize = quantized
        .iter()
        .flat_map(|(_, polys)| polys)
        .flatten()
        .map(std::vec::Vec::len)
        .sum();
    println!(
        "{} epsilon={epsilon_m}m: {} features, {verts} quantized verts, {n_points} points\n",
        dataset.to_uppercase(),
        quantized.len()
    );

    // geo oracle polygons over the identical i64 coords
    let gpolys: Vec<(usize, geo::Polygon<i64>)> = quantized
        .iter()
        .enumerate()
        .flat_map(|(feature_index, (_, polys))| {
            polys.iter().map(move |poly| {
                let ring = |coords: &Vec<(i32, i32)>| -> geo::LineString<i64> {
                    let mut vertices: Vec<(i64, i64)> = coords
                        .iter()
                        .map(|&(x, y)| (i64::from(x), i64::from(y)))
                        .collect();
                    if vertices.first() != vertices.last() {
                        if let Some(&first) = vertices.first() {
                            vertices.push(first);
                        }
                    }
                    vertices.into()
                };
                (
                    feature_index,
                    geo::Polygon::new(ring(&poly[0]), poly[1..].iter().map(ring).collect()),
                )
            })
        })
        .collect();

    let points: Vec<(i32, i32)> = gen_pts(n_points)
        .iter()
        .map(|&(lon, lat)| (q24_lon(lon), q24_lat(lat)))
        .collect();

    // ---- ours: per-polygon integer PIP, linear first-hit scan ----
    let polys: Vec<P> = quantized
        .iter()
        .enumerate()
        .flat_map(|(feature_index, (_, zone_polys))| {
            zone_polys.iter().map(move |poly| {
                let rings: Vec<&[(i32, i32)]> = poly.iter().map(std::vec::Vec::as_slice).collect();
                let mut bbox = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                for &(x, y) in &poly[0] {
                    bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
                }
                P {
                    fi: feature_index,
                    bbox,
                    rings,
                }
            })
        })
        .collect();
    // geometry-rs polygons (expects closed GeoJSON-style rings)
    let geometry_rs_polys: Vec<(usize, geometry_rs::Polygon)> = quantized
        .iter()
        .enumerate()
        .flat_map(|(feature_index, (_, polys))| {
            polys.iter().map(move |poly| {
                let ring = |coords: &Vec<(i32, i32)>| -> Vec<geometry_rs::Point> {
                    let mut vertices: Vec<geometry_rs::Point> = coords
                        .iter()
                        .map(|&(x, y)| geometry_rs::Point {
                            x: f64::from(x),
                            y: f64::from(y),
                        })
                        .collect();
                    let first = vertices[0];
                    vertices.push(first);
                    vertices
                };
                (
                    feature_index,
                    geometry_rs::Polygon::new(
                        ring(&poly[0]),
                        poly[1..].iter().map(ring).collect(),
                        None,
                    ),
                )
            })
        })
        .collect();

    // untimed warmup: first touch of `polys`/`points` — without it the first
    // timed contender eats every cold miss and the shared-structure rows
    // (f64/i128) look spuriously fast
    let (warm, _) = timed_scan(&points, &polys, |i, px, py| {
        utz::pip::contains::<i64, _>(&polys[i].rings, px, py)
    });
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
    let (ours, t_ours) = timed_scan(&points, &polys, |i, px, py| {
        utz::pip::contains::<i64, _>(&polys[i].rings, px, py)
    });
    let (ours_f64, t_f64) = timed_scan(&points, &polys, |i, px, py| {
        utz::pip::contains::<f64, _>(&polys[i].rings, px, py)
    });
    let (ours_i128, t_i128) = timed_scan(&points, &polys, |i, px, py| {
        utz::pip::contains::<i128, _>(&polys[i].rings, px, py)
    });
    let (oracle, t_geo) = timed_scan(&points, &polys, |i, px, py| {
        gpolys[i]
            .1
            .contains(&geo::Point::new(i64::from(px), i64::from(py)))
    });
    let (geometry_rs_answers, t_geometry_rs) = timed_scan(&points, &polys, |i, px, py| {
        geometry_rs_polys[i].1.contains_point(geometry_rs::Point {
            x: f64::from(px),
            y: f64::from(py),
        })
    });

    let mut diff = 0usize;
    for (i, (our_answer, oracle_answer)) in ours.iter().zip(&oracle).enumerate() {
        if our_answer != oracle_answer {
            diff += 1;
            let tzid_of = |answer: &Option<usize>| {
                answer
                    .map(|index| quantized[index].0.clone())
                    .unwrap_or_default()
            };
            println!(
                "  disagree at pt#{i} {:?}: ours={:?} geo={:?}",
                points[i],
                tzid_of(our_answer),
                tzid_of(oracle_answer)
            );
        }
    }
    println!("disagreements vs geo: {diff}/{n_points}");
    // geometry-rs boundary semantics differ (exterior edge = outside, hole
    // edge = inside), so only count — off-boundary answers must still agree
    let geometry_rs_diff = ours
        .iter()
        .zip(&geometry_rs_answers)
        .filter(|(ours_hit, other_hit)| ours_hit != other_hit)
        .count();
    println!(
        "disagreements vs geometry-rs: {geometry_rs_diff}/{n_points} (boundary semantics differ)"
    );
    let f64_diff = ours
        .iter()
        .zip(&ours_f64)
        .filter(|(ours_hit, other_hit)| ours_hit != other_hit)
        .count();
    println!("disagreements vs ours-f64: {f64_diff}/{n_points} (must be 0: f64 exact at i24)");
    let i128_diff = ours
        .iter()
        .zip(&ours_i128)
        .filter(|(ours_hit, other_hit)| ours_hit != other_hit)
        .count();
    println!("disagreements vs ours-i128: {i128_diff}/{n_points} (must be 0: wider is exact)");
    #[expect(
        clippy::cast_precision_loss,
        reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/lookup display"
    )]
    let us = |elapsed: std::time::Duration| elapsed.as_micros() as f64 / n_points as f64;
    println!(
        "ours:        {:>8.2?}  ({:.1} µs/lookup)",
        t_ours,
        us(t_ours)
    );
    println!(
        "ours-f64:    {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_f64,
        us(t_f64),
        t_f64.as_secs_f64() / t_ours.as_secs_f64()
    );
    println!(
        "ours-i128:   {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_i128,
        us(t_i128),
        t_i128.as_secs_f64() / t_ours.as_secs_f64()
    );
    println!(
        "geo:         {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_geo,
        us(t_geo),
        t_geo.as_secs_f64() / t_ours.as_secs_f64()
    );
    println!(
        "geometry-rs: {:>8.2?}  ({:.1} µs/lookup)   ours {:.2}x",
        t_geometry_rs,
        us(t_geometry_rs),
        t_geometry_rs.as_secs_f64() / t_ours.as_secs_f64()
    );
    Ok(())
}

/// Runs the linear first-hit scan of every contender poly with the shared
/// hoisted bbox precheck; this is the scan shell every contender pays
/// identically, and `hit` is the per-candidate containment kernel.
/// Returns the per-point first hit (as a feature index) and the wall
/// time.
fn timed_scan(
    points: &[(i32, i32)],
    polys: &[P],
    hit: impl Fn(usize, i32, i32) -> bool,
) -> (Vec<Option<usize>>, std::time::Duration) {
    let timer = Instant::now();
    let got = points
        .iter()
        .map(|&(px, py)| {
            (0..polys.len())
                .find(|&i| {
                    let poly = &polys[i];
                    px >= poly.bbox.0
                        && py >= poly.bbox.1
                        && px <= poly.bbox.2
                        && py <= poly.bbox.3
                        && hit(i, px, py)
                })
                .map(|i| polys[i].fi)
        })
        .collect();
    (got, timer.elapsed())
}

/// A tzid paired with its polygons, each a list of rings of quantized
/// vertices.
type QFeat = (String, Vec<Vec<Vec<(i32, i32)>>>);

/// Quantizes each feature's per-polygon rings to the i24 grid, dropping
/// degenerate rings, and pairs them with the tzid.
fn quantize(features: &[Feat]) -> Vec<QFeat> {
    features
        .iter()
        .map(|feature| {
            let polys = feature
                .polys
                .iter()
                .map(|poly| {
                    poly.iter()
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
                        .collect::<Vec<_>>()
                })
                .filter(|poly: &Vec<Vec<(i32, i32)>>| !poly.is_empty())
                .collect();
            (feature.tzid.clone().unwrap_or_default(), polys)
        })
        .collect()
}

fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(utz_common::POINT_SEED, n)
}
