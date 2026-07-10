// Topology-aware RDP sweep: for each tolerance, simplify each shared arc once,
// encode Format B (i24 topology + delta/varint), compress, and measure lookup
// accuracy vs the FULL-precision reference over a random sample.
//
// usage: utz-build rdp-sweep [ds]


use geo::Contains;

use utz_build::{topo, Feat};

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let ds = a.ds;
    let feats = utz_build::load(&ds)?;
    let v0: usize = feats.iter().flat_map(|f| &f.polys).flatten().map(std::vec::Vec::len).sum();
    println!("{}: {} features, {v0} vertices\n", ds.to_uppercase(), feats.len());

    // full-precision reference lookups
    let refs = build_refs(&feats);
    let sample = 20_000usize;
    let pts: Vec<(f64, f64)> = gen_pts(sample);
    let truth: Vec<String> = pts.iter().map(|&(lo, la)| lookup(&refs, lo, la)).collect();

    println!("{:>8}{:>10}{:>9}{:>12}{:>12}{:>12}{:>11}",
        "eps(m)", "verts", "%kept", "raw", "zstd22", "xz.dmax", "mismatch");
    println!("{}", "-".repeat(74));

    for eps_m in [0.0f64, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0] {
        let eps_deg = eps_m / 111_320.0;
        let out = topo::encode_topology(&feats, eps_deg);
        let raw = out.bytes.len();
        let z = zstd::encode_all(&out.bytes[..], 22).unwrap().len();
        let x = xz_dmax(&out.bytes);
        // accuracy: reconstructed simplified geometry vs truth
        let srefs = build_refs(&out.simplified);
        let mut miss = 0usize;
        for (i, &(lo, la)) in pts.iter().enumerate() {
            if lookup(&srefs, lo, la) != truth[i] { miss += 1; }
        }
        println!("{:>8}{:>10}{:>8.1}%{:>12}{:>12}{:>12}{:>10.3}%",
            eps_m as u64, out.verts, 100.0 * out.verts as f64 / v0 as f64,
            raw, z, x, 100.0 * miss as f64 / sample as f64);
    }
    Ok(())
}

type Ref = (String, geo::Polygon<f64>);
fn build_refs(feats: &[Feat]) -> Vec<Ref> {
    let mut refs = Vec::new();
    for f in feats {
        let tz = f.tzid.clone().unwrap_or_default();
        for p in &f.polys {
            if p[0].len() < 3 { continue; }
            let ext = geo::LineString::from(closed(&p[0]));
            let holes: Vec<_> = p[1..].iter().filter(|h| h.len() >= 3).map(|h| geo::LineString::from(closed(h))).collect();
            refs.push((tz.clone(), geo::Polygon::new(ext, holes)));
        }
    }
    refs
}
fn closed(v: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut r = v.to_vec();
    if r.first() != r.last() { if let Some(&f) = r.first() { r.push(f); } }
    r
}
fn lookup(refs: &[Ref], lon: f64, lat: f64) -> String {
    let pt = geo::Point::new(lon, lat);
    for (tz, poly) in refs { if poly.contains(&pt) { return tz.clone(); } }
    String::new()
}
fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || { lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407); (lcg >> 11) as f64 / (1u64 << 53) as f64 };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}
fn xz_dmax(raw: &[u8]) -> usize {
    use lzma_rust2::Write as _; // no_std lzma-rust2 XzWriter
    let bits = (usize::BITS - (raw.len().max(1) - 1).leading_zeros()).clamp(12, 26);
    let mut opts = lzma_rust2::XzOptions::with_preset(9);
    opts.lzma_options.dict_size = 1u32 << bits;
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).unwrap();
    w.write_all(raw).unwrap();
    w.finish().unwrap().len()
}
