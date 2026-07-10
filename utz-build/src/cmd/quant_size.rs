// Arc-store encoding shootout (delta+varint vs abs-fixed) at a chosen eps +
// quant grid. usage: utz-build quant-size [eps_m] [qbits...]
use std::io::Write;
use utz_build::topo;

#[derive(clap::Args)]
pub struct Args {
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// quantization widths (16/24/32)
    #[arg(default_values_t = [16u32, 24])]
    qbits: Vec<u32>,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let (eps_m, bits) = (a.eps_m, a.qbits);
    let feats = utz_build::load("now")?;
    let v0: usize = feats.iter().flat_map(|f| &f.polys).flatten().map(std::vec::Vec::len).sum();
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
    use lzma_rust2::Write as _; // no_std lzma-rust2 XzWriter
    let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).unwrap();
    w.write_all(raw).unwrap();
    w.finish().unwrap().len()
}
