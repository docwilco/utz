#![cfg_attr(
    on_docsrs,
    doc = "[`utz_build::Config`]: https://docs.rs/utz_build/latest/utz_build/config/struct.Config.html"
)]
#![cfg_attr(on_docsrs, doc = "")]
//! Encodes an asset to disk from explicit knobs. The asset is the input
//! for `utz_bench_cli`, the embedded bench firmware (which embeds an
//! *uncompressed* asset and borrows it zero-copy from flash via
//! `Finder::from_static()`), and any flash-partition/OTA image.
//! Every flag maps to one [`utz_build::Config`] knob.
//!
//! ```text
//! utz_build_cli gen [ds] [epsilon_m] [--codec none|gzip|zstd|brotli|xz]
//!     [--qbits 24] [--grid-deg 2] [--algorithm rdp|vw|ii|none]
//!     [--geom varint-arcs|fixed-width-arcs|full-rings|coarse]
//!     [--w-min <mult>] [-o out.utz]
//! ```
//!
//! [`utz_build::Config`]: ../../utz_build/config/struct.Config.html

use std::path::PathBuf;

use utz_build::{Codec, Config, Error, GeomEncoding, SimplifyAlgorithm};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The simplification tolerance ceiling in meters.
    #[arg(default_value_t = 500.0)]
    epsilon_m: f64,
    /// The payload codec, one of none|gzip|zstd|brotli|xz (firmware wants
    /// none).
    #[arg(long, default_value = "zstd")]
    codec: String,
    /// The quantization width, one of 16/24/32.
    #[arg(long, default_value_t = 24)]
    qbits: u32,
    /// The grid cell size in degrees (fractional values are allowed, e.g.
    /// 4/3 as 1.333…).
    #[arg(long, default_value_t = 2.0)]
    grid_deg: f64,
    /// The simplification algorithm, one of none|rdp|vw|ii.
    #[arg(long, default_value = "rdp")]
    algorithm: String,
    /// The geometry encoding, one of
    /// varint-arcs|fixed-width-arcs|full-rings|coarse (see `GeomEncoding`
    /// for the size/speed ladder).
    #[arg(long, default_value = "varint-arcs")]
    geom: String,
    /// Enables population weighting with this floor multiplier, strictly
    /// between 0 and 1 (the presets use 0.001-0.10).
    #[arg(long)]
    w_min: Option<f64>,
    /// The output path (the default is `<ds>-<epsilon>m[-w<min>]-<codec>.utz`).
    #[arg(long, short)]
    out: Option<PathBuf>,
}

/// # Errors
/// The command fails on an unknown codec/algorithm/geom name, a dataset
/// load/parse or encode failure, the verify lookup coming back empty, or
/// an I/O error writing the asset and its guard file.
pub fn run(args: Args) -> utz_build::Result<()> {
    let codec = match args.codec.as_str() {
        "none" | "uncompressed" => Codec::Uncompressed,
        "gzip" => Codec::Gzip,
        "zstd" => Codec::Zstd,
        "brotli" => Codec::Brotli,
        "xz" => Codec::Xz,
        other => {
            return Err(Error::Msg(format!(
                "unknown codec {other:?}: use none|gzip|zstd|brotli|xz"
            )))
        }
    };
    let simplify = match args.algorithm.as_str() {
        "none" => SimplifyAlgorithm::None,
        "rdp" => SimplifyAlgorithm::Rdp,
        "vw" | "visvalingam" => SimplifyAlgorithm::Visvalingam,
        "ii" | "imai-iri" => SimplifyAlgorithm::ImaiIri,
        other => {
            return Err(Error::Msg(format!(
                "unknown algorithm {other:?}: use none|rdp|vw|ii"
            )))
        }
    };
    let geom = match args.geom.as_str() {
        "varint-arcs" | "varint" | "delta" => GeomEncoding::VarintArcs,
        "fixed-width-arcs" | "fixed" => GeomEncoding::FixedWidthArcs,
        "full-rings" | "eager" | "image" => GeomEncoding::FullRings,
        "coarse" => GeomEncoding::Coarse,
        other => {
            return Err(Error::Msg(format!(
                "unknown geom {other:?}: use varint-arcs|fixed-width-arcs|full-rings|coarse"
            )))
        }
    };
    let out = args.out.unwrap_or_else(|| {
        let weight_suffix = args
            .w_min
            .map(|floor| format!("-w{floor}"))
            .unwrap_or_default();
        PathBuf::from(format!(
            "{}-{}m{}-{}.utz",
            args.ds, args.epsilon_m, weight_suffix, args.codec
        ))
    });

    let mut config = Config::new()
        .dataset(&args.ds)
        .epsilon_meters(args.epsilon_m)
        .quant_bits(args.qbits)
        .grid_deg(args.grid_deg)
        .codec(codec)
        .simplify_algorithm(simplify)
        .geom(geom)
        .out_path(&out);
    if let Some(floor) = args.w_min {
        config = config.density_weight_floor(floor);
    }
    let path = config.generate()?;

    // sanity: the runtime must accept what we just wrote
    let container = std::fs::read(&path)?;
    let size = container.len();
    let finder = utz::Finder::from_vec(container)?;
    let release = finder.tzbb_release().to_string();
    if finder
        .lookup(utz::Position {
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
