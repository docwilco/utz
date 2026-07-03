// Measurement backlog #4 + #6 (PLAN.md §15): full pipeline size table on the
// REAL container — topology × RDP(ε) × quant(i16/i24) × codec (incl gzip).
//
// usage: cargo run --release -p utz-build --example size_table [osm|osm1970] [grid_deg]

use utz_build::encode::{self, Codec, Params};

fn main() -> anyhow::Result<()> {
    let ds = std::env::args().nth(1).unwrap_or_else(|| "osm".into());
    let grid_deg: u32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(2);
    let feats = utz_build::load(&ds)?;
    println!("{} full container, grid {grid_deg}°, dominant-first CSR", ds.to_uppercase());
    println!("{:>7}{:>6}{:>12}{:>12}{:>12}{:>12}{:>12}",
        "eps(m)", "quant", "raw", "gzip", "zstd22", "br.q11", "xz9");
    println!("{}", "-".repeat(73));

    for eps_m in [100.0f64, 250.0, 500.0, 1000.0, 2000.0] {
        for qbits in [16u32, 24] {
            let p = Params {
                dataset: if ds == "osm" { 0 } else { 1 },
                tzbb_release: "dev",
                eps_m,
                quant_bits: qbits,
                grid_deg,
                codec: Codec::Uncompressed,
            };
            let payload = encode::build_payload(&feats, &p)?;
            let kb = |c: Codec| format!("{:.1}", encode::compress(&payload, c).len() as f64 / 1024.0);
            println!("{:>7}{:>6}{:>11} K{:>11} K{:>11} K{:>11} K{:>11} K",
                eps_m as u64, format!("i{qbits}"),
                format!("{:.1}", payload.len() as f64 / 1024.0),
                kb(Codec::Gzip), kb(Codec::Zstd), kb(Codec::Brotli), kb(Codec::Xz));
        }
    }
    Ok(())
}
