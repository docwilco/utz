//! Encode a container to disk — the input for utz-bench-cli and the
//! ESP32-S3 firmware (which embeds an *uncompressed* container and borrows
//! it zero-copy from flash via `Finder::from_static`).
//!
//! usage: utz-build encode [ds] [eps_m] [--codec none|gzip|zstd|brotli|xz]
//!        [--qbits 24] [--grid-deg 2] [--w-min 0.052] [-o out.utz]

use std::path::PathBuf;

use utz_build::density::DensityGrid;
use utz_build::encode::{self, Codec, Params};
use utz_simplify::DensityWeight;

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance ceiling in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// payload codec: none|gzip|zstd|brotli|xz (firmware wants none)
    #[arg(long, default_value = "zstd")]
    codec: String,
    /// quantization width: 16/24/32
    #[arg(long, default_value_t = 24)]
    qbits: u32,
    /// grid cell size in integer degrees
    #[arg(long, default_value_t = 2.0)]
    grid_deg: f64,
    /// simplification algorithm: rdp|ii (visvalingam: builder API only, §14.8)
    #[arg(long, default_value = "rdp")]
    algo: String,
    /// enable population weighting with this floor multiplier (e.g. 0.052)
    #[arg(long)]
    w_min: Option<f64>,
    /// output path (default: <ds>-<eps>m[-w<min>]-<codec>.utz)
    #[arg(long, short)]
    out: Option<PathBuf>,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let codec = match a.codec.as_str() {
        "none" | "uncompressed" => Codec::Uncompressed,
        "gzip" => Codec::Gzip,
        "zstd" => Codec::Zstd,
        "brotli" => Codec::Brotli,
        "xz" => Codec::Xz,
        c => anyhow::bail!("unknown codec {c:?}: use none|gzip|zstd|brotli|xz"),
    };
    let simplify = match a.algo.as_str() {
        "rdp" => encode::SimplifyAlgo::Rdp,
        "ii" | "imai-iri" => encode::SimplifyAlgo::ImaiIri,
        c => anyhow::bail!("unknown algo {c:?}: use rdp|ii (visvalingam needs the builder API)"),
    };
    let (feats, release) = utz_build::load_with_release(&a.ds)?;
    let p = Params {
        dataset: utz_build::dataset(&a.ds)?.code(),
        tzbb_release: &release,
        eps_m: a.eps_m,
        quant_bits: a.qbits,
        grid_deg: a.grid_deg,
        codec,
        simplify,
    };
    let container = match a.w_min {
        Some(w) => {
            let grid = DensityGrid::load(&utz_build::cache_dir())?;
            utz_build::encode_weighted(&feats, &p, &grid, DensityWeight::new(w))?
        }
        None => encode::encode(&feats, &p)?,
    };

    // sanity: the runtime must accept what we just wrote
    let f = utz::Finder::from_vec(container.clone()).map_err(|e| anyhow::anyhow!("verify: {e}"))?;
    anyhow::ensure!(f.lookup(utz::Position { lon: -0.1276, lat: 51.5072 }).is_some(), "verify lookup failed");

    let out = a.out.unwrap_or_else(|| {
        let w = a.w_min.map(|w| format!("-w{w}")).unwrap_or_default();
        PathBuf::from(format!("{}-{}m{}-{}.utz", a.ds, a.eps_m, w, a.codec))
    });
    std::fs::write(&out, &container)?;
    println!("wrote {} ({:.1} KiB, {codec:?}, TZBB {release})", out.display(), container.len() as f64 / 1024.0);
    Ok(())
}
