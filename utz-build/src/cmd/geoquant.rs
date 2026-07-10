// Does geo's integer PIP agree with its f64 PIP? Tests overflow behaviour of the
// SimpleKernel at different coord types/grids on OSM -now.
use geo::Contains;
use geo_types::{LineString, Point, Polygon};

#[derive(clap::Args)]
pub struct Args {}

pub fn run(_a: Args) -> utz_build::Result<()> {
    // load geometry once (f64), build parallel i64 / i32 copies at deg*1e6 (~0.11 m)
    let mut f64p: Vec<(String, Polygon<f64>)> = Vec::new();
    let mut i64p: Vec<(String, Polygon<i64>)> = Vec::new();
    let mut i32p: Vec<(String, Polygon<i32>)> = Vec::new();
    for f in utz_build::load("now")? {
        let tz = f.tzid.clone().unwrap_or_default();
        for p in &f.polys {
            let ext = p[0].clone();
            let holes: Vec<Vec<(f64, f64)>> = p[1..].to_vec();
            f64p.push((tz.clone(), poly_f64(&ext, &holes)));
            #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 fits i64")]
            i64p.push((tz.clone(), poly_i(&ext, &holes, 1e6, |v| v as i64)));
            #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 (±1.8e8) fits i32")]
            i32p.push((tz.clone(), poly_i(&ext, &holes, 1e6, |v| v as i32)));
        }
    }
    println!("polys={}\n", f64p.len());

    let look_f64 = |lo: f64, la: f64| -> String { let pt = Point::new(lo, la);
        for (tz, p) in &f64p { if p.contains(&pt) { return tz.clone(); } } String::new() };
    #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 fits i64")]
    let look_i64 = |lo: f64, la: f64| -> String { let pt = Point::new((lo * 1e6).round() as i64, (la * 1e6).round() as i64);
        for (tz, p) in &i64p { if p.contains(&pt) { return tz.clone(); } } String::new() };
    #[expect(clippy::cast_possible_truncation, reason = "rounded deg*1e6 (±1.8e8) fits i32")]
    let look_i32 = |lo: f64, la: f64| -> String { let pt = Point::new((lo * 1e6).round() as i32, (la * 1e6).round() as i32);
        for (tz, p) in &i32p { if p.contains(&pt) { return tz.clone(); } } String::new() };

    let mut lcg = 0x9e37_79b9_7f4a_7c15u64;
    #[expect(clippy::cast_precision_loss, reason = "53-bit mantissa construction: lcg>>11 < 2^53 and 2^53 are both exact")]
    let mut next = || { lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    let (mut n, mut d64, mut d32) = (0u64, 0u64, 0u64);
    while n < 8000 {
        let lo = next() * 360.0 - 180.0; let la = next() * 180.0 - 90.0; n += 1;
        let t = look_f64(lo, la);
        if look_i64(lo, la) != t { d64 += 1; }
        if look_i32(lo, la) != t { d32 += 1; }
    }
    println!("{n} points, vs geo-f64 as reference:");
    #[expect(clippy::cast_precision_loss, reason = "d64/d32 ≤ n = 8000 sample points; percentage display")]
    let (p64, p32) = (100.0 * d64 as f64 / n as f64, 100.0 * d32 as f64 / n as f64);
    println!("  geo-i64 (deg*1e6) disagreements: {d64}  ({p64:.3}%)");
    println!("  geo-i32 (deg*1e6) disagreements: {d32}  ({p32:.3}%)  <- overflow in orient2d");
    Ok(())
}

fn poly_f64(ext: &[(f64, f64)], holes: &[Vec<(f64, f64)>]) -> Polygon<f64> {
    Polygon::new(LineString::from(ext.to_vec()), holes.iter().map(|h| LineString::from(h.clone())).collect())
}
fn poly_i<T: geo_types::CoordNum>(ext: &[(f64, f64)], holes: &[Vec<(f64, f64)>], s: f64, f: impl Fn(f64) -> T + Copy) -> Polygon<T> {
    let cv = |v: &[(f64, f64)]| -> LineString<T> { LineString::from(v.iter().map(|&(x, y)| (f((x * s).round()), f((y * s).round()))).collect::<Vec<_>>()) };
    Polygon::new(cv(ext), holes.iter().map(|h| cv(h)).collect())
}
