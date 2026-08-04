#![cfg_attr(
    on_docsrs,
    doc = "[`utz_build::Config`]: https://docs.rs/utz_build/latest/utz_build/config/struct.Config.html"
)]
#![cfg_attr(on_docsrs, doc = "")]
//! Encode an asset to disk from explicit knobs: the input for
//! `utz_bench_cli`, the embedded bench firmware (which embeds an
//! *uncompressed* asset and borrows it zero-copy from flash via
//! `Finder::from_static()`), and any flash-partition/OTA image.
//! Every flag maps to one [`utz_build::Config`] knob.
//!
//! ```text
//! utz_build_cli gen [ds] [eps_m] [--codec none|gzip|zstd|brotli|xz]
//!     [--qbits 24] [--grid-deg 2] [--algo rdp|vw|ii|none]
//!     [--geom varint-arcs|fixed-width-arcs|full-rings|coarse]
//!     [--w-min <mult>] [-o out.utz]
//! ```
//!
//! [`utz_build::Config`]: ../../utz_build/config/struct.Config.html

use std::path::PathBuf;

use utz_build::{Codec, Config, Error, GeomEncoding, SimplifyAlgo};

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
    /// grid cell size in degrees (fractional allowed, e.g. 4/3 as 1.333…)
    #[arg(long, default_value_t = 2.0)]
    grid_deg: f64,
    /// simplification algorithm: none|rdp|vw|ii
    #[arg(long, default_value = "rdp")]
    algo: String,
    /// geometry encoding: varint-arcs|fixed-width-arcs|full-rings|coarse
    /// (see `GeomEncoding` for the size/speed ladder)
    #[arg(long, default_value = "varint-arcs")]
    geom: String,
    /// enable population weighting with this floor multiplier, strictly
    /// between 0 and 1 (the presets use 0.001-0.10)
    #[arg(long)]
    w_min: Option<f64>,
    /// output path (default: `<ds>-<eps>m[-w<min>]-<codec>.utz`)
    #[arg(long, short)]
    out: Option<PathBuf>,
}

/// # Errors
/// Unknown codec/algo/geom name, dataset load/parse or encode failure, the
/// verify lookup coming back empty, or I/O writing the asset and its guard
/// file.
pub fn run(a: Args) -> utz_build::Result<()> {
    let codec = match a.codec.as_str() {
        "none" | "uncompressed" => Codec::Uncompressed,
        "gzip" => Codec::Gzip,
        "zstd" => Codec::Zstd,
        "brotli" => Codec::Brotli,
        "xz" => Codec::Xz,
        c => {
            return Err(Error::Msg(format!(
                "unknown codec {c:?}: use none|gzip|zstd|brotli|xz"
            )))
        }
    };
    let simplify = match a.algo.as_str() {
        "none" => SimplifyAlgo::None,
        "rdp" => SimplifyAlgo::Rdp,
        "vw" | "visvalingam" => SimplifyAlgo::Visvalingam,
        "ii" | "imai-iri" => SimplifyAlgo::ImaiIri,
        c => {
            return Err(Error::Msg(format!(
                "unknown algo {c:?}: use none|rdp|vw|ii"
            )))
        }
    };
    let geom = match a.geom.as_str() {
        "varint-arcs" | "varint" | "delta" => GeomEncoding::VarintArcs,
        "fixed-width-arcs" | "fixed" => GeomEncoding::FixedWidthArcs,
        "full-rings" | "eager" | "image" => GeomEncoding::FullRings,
        "coarse" => GeomEncoding::Coarse,
        c => {
            return Err(Error::Msg(format!(
                "unknown geom {c:?}: use varint-arcs|fixed-width-arcs|full-rings|coarse"
            )))
        }
    };
    let out = a.out.unwrap_or_else(|| {
        let w = a.w_min.map(|w| format!("-w{w}")).unwrap_or_default();
        PathBuf::from(format!("{}-{}m{}-{}.utz", a.ds, a.eps_m, w, a.codec))
    });

    let mut config = Config::new()
        .dataset(&a.ds)
        .rdp_meters(a.eps_m)
        .quant_bits(a.qbits)
        .grid_deg(a.grid_deg)
        .codec(codec)
        .simplify_algo(simplify)
        .geom(geom)
        .out_path(&out);
    if let Some(w) = a.w_min {
        config = config.density_weight_floor(w);
    }
    let path = config.generate()?;

    // sanity: the runtime must accept what we just wrote
    let container = std::fs::read(&path)?;
    let size = container.len();
    let f = utz::Finder::from_vec(container)?;
    let release = f.tzbb_release().to_string();
    if f.lookup(utz::Position {
        lon: -0.1276,
        lat: 51.5072,
    })?
    .is_none()
    {
        // don't leave a bad asset (or its guard) where a good one may
        // have been
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("utz.guard.rs"));
        return Err(Error::Msg("verify lookup failed".into()));
    }
    #[expect(clippy::cast_precision_loss, reason = "asset size ≪ 2^53; KiB display")]
    let kib = size as f64 / 1024.0;
    println!(
        "wrote {} ({kib:.1} KiB, {codec:?}, TZBB {release})",
        path.display()
    );
    Ok(())
}
