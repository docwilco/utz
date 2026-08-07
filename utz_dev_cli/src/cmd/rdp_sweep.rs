//! Sweeps topology-aware RDP tolerances: for each tolerance the command
//! simplifies each shared arc once, encodes the shared-arc payload (i24
//! topology + delta/varint), compresses it, and measures lookup accuracy
//! against the full-precision reference over a random sample.
//!
//! ```text
//! utz_dev_cli rdp-sweep [ds]
//! ```

use geo::Contains;

use utz_encode::{Feat, topo};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
///
/// # Panics
/// The command panics if zstd compression of the encoded payload fails.
pub fn run(args: Args) -> utz_build::Result<()> {
    let dataset = args.ds;
    let features = utz_build::load(&dataset)?;
    let raw_verts: usize = features
        .iter()
        .flat_map(|feature| &feature.polys)
        .flatten()
        .map(std::vec::Vec::len)
        .sum();
    println!(
        "{}: {} features, {raw_verts} vertices\n",
        dataset.to_uppercase(),
        features.len()
    );

    // full-precision reference lookups
    let refs = build_refs(&features);
    let sample = 20_000usize;
    let points: Vec<(f64, f64)> = gen_pts(sample);
    let truth: Vec<String> = points
        .iter()
        .map(|&(lon, lat)| lookup(&refs, lon, lat))
        .collect();

    println!(
        "{:>8}{:>10}{:>9}{:>12}{:>12}{:>12}{:>11}",
        "epsilon(m)", "verts", "%kept", "raw", "zstd22", "xz.dmax", "mismatch"
    );
    println!("{}", "-".repeat(74));

    for epsilon_m in [0.0f64, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0] {
        let epsilon_deg = epsilon_m / utz_common::METERS_PER_DEG;
        let out = topo::encode_topology(&features, epsilon_deg);
        let raw = out.bytes.len();
        let zstd_len = zstd::encode_all(&out.bytes[..], 22).unwrap().len();
        let xz_len = xz_dmax(&out.bytes);
        // accuracy: reconstructed simplified geometry vs truth
        let simplified_refs = build_refs(&out.simplified);
        let mut miss = 0usize;
        for (i, &(lon, lat)) in points.iter().enumerate() {
            if lookup(&simplified_refs, lon, lat) != truth[i] {
                miss += 1;
            }
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "vertex counts and miss ≤ sample point count ≪ 2^53; % display"
        )]
        let (kept_pct, miss_pct) = (
            100.0 * out.verts as f64 / raw_verts as f64,
            100.0 * miss as f64 / sample as f64,
        );
        println!(
            "{:>8}{:>10}{kept_pct:>8.1}%{:>12}{:>12}{:>12}{miss_pct:>10.3}%",
            epsilon_m, out.verts, raw, zstd_len, xz_len
        );
    }
    Ok(())
}

type Ref = (String, geo::Polygon<f64>);
fn build_refs(features: &[Feat]) -> Vec<Ref> {
    let mut refs = Vec::new();
    for feature in features {
        let tzid = feature.tzid.clone().unwrap_or_default();
        for poly in &feature.polys {
            if poly[0].len() < 3 {
                continue;
            }
            let exterior = geo::LineString::from(closed(&poly[0]));
            let holes: Vec<_> = poly[1..]
                .iter()
                .filter(|hole| hole.len() >= 3)
                .map(|hole| geo::LineString::from(closed(hole)))
                .collect();
            refs.push((tzid.clone(), geo::Polygon::new(exterior, holes)));
        }
    }
    refs
}
fn closed(ring: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut result = ring.to_vec();
    if result.first() != result.last()
        && let Some(&first) = result.first()
    {
        result.push(first);
    }
    result
}
fn lookup(refs: &[Ref], lon: f64, lat: f64) -> String {
    let point = geo::Point::new(lon, lat);
    for (tzid, poly) in refs {
        if poly.contains(&point) {
            return tzid.clone();
        }
    }
    String::new()
}
fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(utz_common::POINT_SEED, n)
}
fn xz_dmax(raw: &[u8]) -> usize {
    use lzma_rust2::Write as _; // no_std lzma-rust2 XzWriter
    let bits = (usize::BITS - (raw.len().max(1) - 1).leading_zeros()).clamp(12, 26);
    let mut options = lzma_rust2::XzOptions::with_preset(9);
    options.lzma_options.dict_size = 1u32 << bits;
    let mut writer = lzma_rust2::XzWriter::new(Vec::new(), options).unwrap();
    writer.write_all(raw).unwrap();
    writer.finish().unwrap().len()
}
