//! Does geo's integer PIP agree with its f64 PIP? The command tests
//! overflow behaviour of the `SimpleKernel` at different coord types/grids
//! on the `now` dataset.

use geo::Contains;
use geo_types::{LineString, Point, Polygon};

#[derive(clap::Args)]
pub struct Args {}

/// # Errors
/// The command fails on a dataset load/parse failure.
pub fn run(_args: Args) -> utz_build::Result<()> {
    // load geometry once (f64), build parallel i64 / i32 copies at deg*1e6 (~0.11 m)
    let mut f64_polys: Vec<(String, Polygon<f64>)> = Vec::new();
    let mut i64_polys: Vec<(String, Polygon<i64>)> = Vec::new();
    let mut i32_polys: Vec<(String, Polygon<i32>)> = Vec::new();
    for feature in utz_build::load("now")? {
        let tzid = feature.tzid.clone().unwrap_or_default();
        for poly in &feature.polys {
            let exterior = poly[0].clone();
            let holes: Vec<Vec<(f64, f64)>> = poly[1..].to_vec();
            f64_polys.push((tzid.clone(), poly_f64(&exterior, &holes)));
            #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 fits i64")]
            i64_polys.push((
                tzid.clone(),
                poly_i(&exterior, &holes, 1e6, |value| value as i64),
            ));
            #[expect(
                clippy::cast_possible_truncation,
                reason = "rounded deg*1e6 (±1.8e8) fits i32"
            )]
            i32_polys.push((
                tzid.clone(),
                poly_i(&exterior, &holes, 1e6, |value| value as i32),
            ));
        }
    }
    println!("polys={}\n", f64_polys.len());

    #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 fits i64")]
    let q64 = |value: f64| (value * 1e6).round() as i64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "rounded deg*1e6 (±1.8e8) fits i32"
    )]
    let q32 = |value: f64| (value * 1e6).round() as i32;

    let mut lcg = utz_common::Lcg::new(0x9e37_79b9_7f4a_7c15);
    let mut next = || lcg.unit_f64();
    let (mut n_points, mut diff_i64, mut diff_i32) = (0u64, 0u64, 0u64);
    while n_points < 8000 {
        let lon = next() * 360.0 - 180.0;
        let lat = next() * 180.0 - 90.0;
        n_points += 1;
        let truth = look(&f64_polys, Point::new(lon, lat));
        if look(&i64_polys, Point::new(q64(lon), q64(lat))) != truth {
            diff_i64 += 1;
        }
        if look(&i32_polys, Point::new(q32(lon), q32(lat))) != truth {
            diff_i32 += 1;
        }
    }
    println!("{n_points} points, vs geo-f64 as reference:");
    #[expect(
        clippy::cast_precision_loss,
        reason = "diff_i64/diff_i32 ≤ n_points = 8000 sample points; percentage display"
    )]
    let (pct_i64, pct_i32) = (
        100.0 * diff_i64 as f64 / n_points as f64,
        100.0 * diff_i32 as f64 / n_points as f64,
    );
    println!("  geo-i64 (deg*1e6) disagreements: {diff_i64}  ({pct_i64:.3}%)");
    println!(
        "  geo-i32 (deg*1e6) disagreements: {diff_i32}  ({pct_i32:.3}%)  <- overflow in orient2d"
    );
    Ok(())
}

/// First zone whose polygon contains `point`: one lookup for every width.
fn look<T: geo::GeoNum>(polys: &[(String, Polygon<T>)], point: Point<T>) -> String {
    polys
        .iter()
        .find(|(_, poly)| poly.contains(&point))
        .map(|(tzid, _)| tzid.clone())
        .unwrap_or_default()
}

fn poly_f64(exterior: &[(f64, f64)], holes: &[Vec<(f64, f64)>]) -> Polygon<f64> {
    Polygon::new(
        LineString::from(exterior.to_vec()),
        holes
            .iter()
            .map(|hole| LineString::from(hole.clone()))
            .collect(),
    )
}
fn poly_i<T: geo_types::CoordNum>(
    exterior: &[(f64, f64)],
    holes: &[Vec<(f64, f64)>],
    scale: f64,
    convert: impl Fn(f64) -> T + Copy,
) -> Polygon<T> {
    let convert_ring = |ring: &[(f64, f64)]| -> LineString<T> {
        LineString::from(
            ring.iter()
                .map(|&(x, y)| (convert((x * scale).round()), convert((y * scale).round())))
                .collect::<Vec<_>>(),
        )
    };
    Polygon::new(
        convert_ring(exterior),
        holes.iter().map(|hole| convert_ring(hole)).collect(),
    )
}
