//! Runs the arc-store encoding shootout (delta+varint vs abs-fixed) at a
//! chosen eps and quant grid.
//!
//! ```text
//! utz_dev_cli quant-size [eps_m] [qbits...]
//! ```
use std::io::Write;
use utz_encode::topo;

#[derive(clap::Args)]
pub struct Args {
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// The quantization widths (16/24/32).
    #[arg(default_values_t = [16u32, 24])]
    qbits: Vec<u32>,
}

/// # Errors
/// The command fails on a dataset load/parse failure.
///
/// # Panics
/// The command panics if zstd compression of the encoded arc store fails.
pub fn run(args: Args) -> utz_build::Result<()> {
    let (eps_m, quant_widths) = (args.eps_m, args.qbits);
    let features = utz_build::load("now")?;
    let raw_verts: usize = features
        .iter()
        .flat_map(|feature| &feature.polys)
        .flatten()
        .map(std::vec::Vec::len)
        .sum();
    println!(
        "with-oceans-now: {} features, {raw_verts} verts",
        features.len()
    );
    println!("topology + topology-aware RDP eps={eps_m} m\n");
    println!(
        "{:<16}{:>10}{:>12}{:>12}{:>12}{:>12}",
        "encoding", "arc-verts", "raw", "zstd22", "br.w24", "xz.dmax"
    );
    println!("{}", "-".repeat(74));
    let eps_deg = eps_m / 111_320.0;
    for &quant_bits in &quant_widths {
        for (tag, abs_fixed) in [("delta+varint", false), ("abs-fixed", true)] {
            let out = topo::encode_topology_qm(&features, eps_deg, quant_bits, abs_fixed);
            let raw = &out.bytes;
            let zstd_len = zstd::encode_all(&raw[..], 22).unwrap().len();
            let brotli_len = brotli_w24(raw);
            let xz_len = xz_dmax(raw);
            let name = format!("i{quant_bits} {tag}");
            println!(
                "{:<16}{:>10}{:>12}{:>12}{:>12}{:>12}",
                name,
                out.verts,
                raw.len(),
                zstd_len,
                brotli_len,
                xz_len
            );
        }
    }
    Ok(())
}

fn brotli_w24(raw: &[u8]) -> usize {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 11,
        lgwin: 24,
        ..Default::default()
    };
    let mut out = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::with_params(&mut out, 4096, &params);
        writer.write_all(raw).unwrap();
    }
    out.len()
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
