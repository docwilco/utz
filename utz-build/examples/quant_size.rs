// Arc-store encoding shootout (delta+varint vs abs-fixed) at a chosen eps +
// quant grid. usage: cargo run --release --example quant_size [eps_m] [qbits...]
use std::io::Write;
use utz_build::topo;

fn main() -> anyhow::Result<()> {
    let eps_m: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(500.0);
    let bits: Vec<u32> = {
        let v: Vec<u32> = std::env::args().skip(2).filter_map(|s| s.parse().ok()).collect();
        if v.is_empty() { vec![16, 24] } else { v }
    };
    let feats = utz_build::load("now")?;
    let v0: usize = feats.iter().flat_map(|f| &f.polys).flat_map(|p| p).map(|r| r.len()).sum();
    println!("with-oceans-now: {} features, {v0} verts", feats.len());
    println!("topology + topology-aware RDP eps={eps_m} m\n");
    println!("{:<16}{:>10}{:>12}{:>12}{:>12}{:>12}", "encoding", "arc-verts", "raw", "zstd22", "br.w24", "xz.dmax");
    println!("{}", "-".repeat(74));
    let eps_deg = eps_m / 111_320.0;
    for &qbits in &bits {
        for (tag, abs_fixed) in [("delta+varint", false), ("abs-fixed", true)] {
            let out = topo::encode_topology_qm(&feats, eps_deg, qbits, abs_fixed);
            let raw = &out.bytes;
            let z = zstd::encode_all(&raw[..], 22).unwrap().len();
            let b = brotli_w24(raw);
            let x = xz_dmax(raw);
            let name = format!("i{qbits} {tag}");
            println!("{:<16}{:>10}{:>12}{:>12}{:>12}{:>12}", name, out.verts, raw.len(), z, b, x);
        }
    }
    Ok(())
}

fn brotli_w24(raw: &[u8]) -> usize {
    let params = brotli::enc::BrotliEncoderParams { quality: 11, lgwin: 24, ..Default::default() };
    let mut out = Vec::new();
    { let mut w = brotli::CompressorWriter::with_params(&mut out, 4096, &params); w.write_all(raw).unwrap(); }
    out.len()
}
fn xz_dmax(raw: &[u8]) -> usize {
    let bits = (usize::BITS - (raw.len().max(1) - 1).leading_zeros()).clamp(12, 26);
    let mut opts = lzma_rust2::XzOptions::with_preset(9);
    opts.lzma_options.dict_size = 1u32 << bits;
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).unwrap();
    w.write_all(raw).unwrap();
    w.finish().unwrap().len()
}
