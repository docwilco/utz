//! Prints the full pipeline size table on the *real* asset, covering
//! topology × RDP(ε) × quant(i16/i24) × codec (including gzip).
//!
//! ```text
//! utz_dev_cli size-table [ds] [grid_deg]
//! ```

use utz_encode::encode::{self, Codec, Params};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The grid cell size in integer degrees.
    #[arg(default_value_t = 2.0)]
    grid_deg: f64,
}

/// # Errors
/// The command fails on a dataset load/parse, payload encode, or
/// compression backend failure.
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, grid_deg) = (args.ds, args.grid_deg);
    let features = utz_build::load(&dataset)?;
    println!(
        "{} full container, grid {grid_deg}°, dominant-first CSR",
        dataset.to_uppercase()
    );
    println!(
        "{:>7}{:>6}{:>12}{:>12}{:>12}{:>12}{:>12}",
        "epsilon(m)", "quant", "raw", "gzip", "zstd22", "br.q11", "xz9"
    );
    println!("{}", "-".repeat(73));

    for epsilon_m in [100.0f64, 250.0, 500.0, 1000.0, 2000.0] {
        for quant_bits in [16u32, 24] {
            let params = Params {
                dataset: utz_build::dataset(&dataset)?.code(),
                tzbb_release: "dev",
                epsilon_m,
                quant_bits,
                grid_deg,
                codec: Codec::Uncompressed,
                simplify: encode::SimplifyAlgorithm::default(),
                geom: encode::GeomEncoding::default(),
                density_weight_floor: None,
            };
            let payload = encode::build_payload(&features, &params)?;
            #[expect(
                clippy::cast_precision_loss,
                reason = "compressed payload size ≪ 2^53; KiB display"
            )]
            let kb = |codec: Codec| -> utz_build::Result<String> {
                Ok(format!(
                    "{:.1}",
                    encode::compress(&payload, codec)?.len() as f64 / 1024.0
                ))
            };
            #[expect(
                clippy::cast_precision_loss,
                reason = "raw payload size ≪ 2^53; KiB display"
            )]
            let raw_kb = payload.len() as f64 / 1024.0;
            println!(
                "{:>7}{:>6}{:>11} K{:>11} K{:>11} K{:>11} K{:>11} K",
                epsilon_m,
                format!("i{quant_bits}"),
                format!("{raw_kb:.1}"),
                kb(Codec::Gzip)?,
                kb(Codec::Zstd)?,
                kb(Codec::Brotli)?,
                kb(Codec::Xz)?
            );
        }
    }
    Ok(())
}
