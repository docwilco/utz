//! Consumer-facing builder API for the `custom` tier (PLAN.md §11): the
//! typed config IS the build config — rustdoc'd, IDE-completable, no file
//! discovery. Meant for a consumer `build.rs` with `utz-build` as a
//! build-dependency (`prost-build` pattern):
//!
//! ```no_run
//! // build.rs
//! let out = utz_build::Config::new()
//!     .dataset("now")
//!     .rdp_meters(500.0)
//!     .generate()
//!     .unwrap();
//! // then in the crate:
//! //   static TZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tz.utz"));
//! //   let finder = utz::Finder::from_slice(TZ)?;
//! ```
//!
//! Source data (TZBB, optionally GHS-POP for density weighting) is fetched
//! into the cache, never committed (§5); downloads are cond-GET-cached so
//! regeneration is cheap.
//!
//! The preset recipes (§14.5) double as constructors — start from one and
//! override a single knob instead of spelling the whole recipe:
//! `Config::compact().codec(Codec::Uncompressed)`.

use std::path::PathBuf;

use crate::encode::{self, Codec, GeomEncoding, Params, SimplifyAlgo};

/// Builder for a custom `.utz` asset. Defaults: dataset `now`, RDP ε=500 m,
/// i24, 2° grid, gzip.
#[derive(Clone, Debug)]
pub struct Config {
    dataset: String,
    eps_m: f64,
    quant_bits: u32,
    grid_deg: f64,
    codec: Codec,
    simplify: SimplifyAlgo,
    geom: GeomEncoding,
    density_weight_floor: Option<f64>,
    out: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dataset: "now".into(),
            eps_m: 500.0,
            quant_bits: 24,
            grid_deg: 2.0,
            codec: Codec::Gzip,
            simplify: SimplifyAlgo::Rdp,
            geom: GeomEncoding::DeltaVarint,
            density_weight_floor: None,
            out: None,
        }
    }
}

impl Config {
    #[must_use]
    pub fn new() -> Self {
        Config::default()
    }

    /// The `tiny` preset recipe (§14.5): RDP ε=10 000 m with pop-density
    /// floor 1e-3, i16, 2° grid, gzip. A preset constructor is a starting
    /// point for one-knob variants — `tiny-static` is
    /// `Config::tiny().codec(Codec::Uncompressed)`.
    #[must_use]
    pub fn tiny() -> Self {
        Config::new()
            .rdp_meters(10_000.0)
            .density_weight_floor(0.001)
            .quant_bits(16)
            .grid_deg(2.0)
            .codec(Codec::Gzip)
    }

    /// The `compact` preset recipe (§14.5): RDP ε=1 000 m with pop-density
    /// floor 1e-3, i24, 4/3° grid, xz.
    #[must_use]
    pub fn compact() -> Self {
        Config::new()
            .rdp_meters(1_000.0)
            .density_weight_floor(0.001)
            .quant_bits(24)
            .grid_deg(4.0 / 3.0)
            .codec(Codec::Xz)
    }

    /// The `balanced` preset recipe (§14.5): RDP ε=50 m with pop-density
    /// floor 2e-2, i24, 2/3° grid, brotli.
    #[must_use]
    pub fn balanced() -> Self {
        Config::new()
            .rdp_meters(50.0)
            .density_weight_floor(0.020)
            .quant_bits(24)
            .grid_deg(2.0 / 3.0)
            .codec(Codec::Brotli)
    }

    /// The `accurate` preset recipe (§14.5): RDP ε=10 m with pop-density
    /// floor 1e-1, i32, 0.5° grid, brotli.
    #[must_use]
    pub fn accurate() -> Self {
        Config::new()
            .rdp_meters(10.0)
            .density_weight_floor(0.10)
            .quant_bits(32)
            .grid_deg(0.5)
            .codec(Codec::Brotli)
    }

    /// Dataset: `[land-]now|1970|all` (§6).
    #[must_use]
    pub fn dataset(mut self, ds: &str) -> Self {
        self.dataset = ds.into();
        self
    }

    /// Simplification tolerance ceiling in meters (RDP ε).
    #[must_use]
    pub fn rdp_meters(mut self, eps_m: f64) -> Self {
        self.eps_m = eps_m;
        self
    }

    /// Quantization width: 16 / 24 / 32 (§8).
    #[must_use]
    pub fn quant_bits(mut self, bits: u32) -> Self {
        self.quant_bits = bits;
        self
    }

    /// Grid cell size in degrees, 0.1–45 (§10).
    #[must_use]
    pub fn grid_deg(mut self, deg: f64) -> Self {
        self.grid_deg = deg;
        self
    }

    /// Payload codec (§7). `Codec::Uncompressed` gives a `core`-rung asset:
    /// zero decode RAM, more flash.
    #[must_use]
    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Simplification algorithm (§14.8). Default RDP; `ImaiIri` gives provably
    /// minimum vertices for the same ε (−4 to −19% measured, slower encode).
    #[must_use]
    pub fn simplify_algo(mut self, algo: SimplifyAlgo) -> Self {
        self.simplify = algo;
        self
    }

    /// Arc-store encoding (§13/§15). Default delta+varint (smallest flash).
    /// `GeomEncoding::Fixed` stores absolute fixed-width coords: +40–72% raw
    /// / +24–32% best-compressed flash, and streaming lookups skip the
    /// per-vertex varint decode — near-eager speed with zero RAM cache, the
    /// XIP `-static` embedded tier.
    #[must_use]
    pub fn geom(mut self, geom: GeomEncoding) -> Self {
        self.geom = geom;
        self
    }

    /// Population-density-weighted simplification: ε multiplier floor in the
    /// densest cells (tiny uses 1e-3). First use downloads GHS-POP (~460 MB,
    /// cached).
    #[must_use]
    pub fn density_weight_floor(mut self, w_min: f64) -> Self {
        self.density_weight_floor = Some(w_min);
        self
    }

    /// Where to write the asset. Default: `$OUT_DIR/tz.utz` (build.rs context).
    pub fn out_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.out = Some(p.into());
        self
    }

    /// Fetch sources (cached), build the container, write it, return the
    /// path. Also writes `<out>.guard.rs` — a compile-time assertion of the
    /// `utz` features this asset needs (via [`utz::caps`]); `include!` it
    /// next to the `include_bytes!` so a feature mismatch fails the build
    /// instead of the first load.
    pub fn generate(self) -> anyhow::Result<PathBuf> {
        let (feats, release) = crate::load_with_release(&self.dataset)?;
        let p = Params {
            dataset: crate::dataset(&self.dataset)?.code(),
            tzbb_release: &release,
            eps_m: self.eps_m,
            quant_bits: self.quant_bits,
            grid_deg: self.grid_deg,
            codec: self.codec,
            simplify: self.simplify,
            geom: self.geom,
        };
        let bytes = match self.density_weight_floor {
            Some(w) => {
                // TODO(hermetic consumers, §11): cache_dir() is workspace-
                // relative — as a build-dependency this lands in the registry
                // copy. Needs a user-cache dir + a pre-fetched-source knob.
                let grid = crate::density::DensityGrid::load(&crate::cache_dir())?;
                crate::encode_weighted(&feats, &p, &grid, utz_simplify::DensityWeight::new(w))?
            }
            None => encode::encode(&feats, &p)?,
        };
        let out = if let Some(p) = self.out { p } else {
            let dir = std::env::var_os("OUT_DIR")
                .ok_or_else(|| anyhow::anyhow!("no OUT_DIR (not in a build.rs?) — set .out_path()"))?;
            PathBuf::from(dir).join("tz.utz")
        };
        std::fs::write(&out, &bytes)?;
        write_guard(&out, self.geom, self.codec)?;
        Ok(out)
    }
}

/// Emit `<asset>.guard.rs`: `const _` assertions against [`utz::caps`] for
/// the geometry decoder and codec the asset needs (shared by
/// [`Config::generate`] and the `gen` CLI).
pub fn write_guard(out: &std::path::Path, geom: GeomEncoding, codec: Codec) -> anyhow::Result<()> {
    let name = out.file_name().and_then(|n| n.to_str()).unwrap_or("asset");
    let (gc, gf) = match geom {
        GeomEncoding::DeltaVarint => ("GEOM_VARINT", "geom-varint"),
        GeomEncoding::Fixed => ("GEOM_FIXED", "geom-fixed"),
        GeomEncoding::EagerImage => ("GEOM_IMAGE", "geom-image"),
        GeomEncoding::Coarse => ("GEOM_COARSE", "geom-coarse"),
    };
    let mut g = format!(
        "// generated by utz-build alongside {name}: include! this next to the\n\
         // include_bytes! to turn a missing-feature load error into a compile error.\n\
         const _: () = assert!(utz::caps::{gc}, \"{name} needs the utz feature `{gf}`\");\n"
    );
    let codec_guard = match codec {
        Codec::Uncompressed => None,
        Codec::Gzip => Some(("GZIP", "gzip")),
        Codec::Zstd => Some(("ZSTD", "ruzstd` or `zstd-sys")),
        Codec::Brotli => Some(("BROTLI", "brotli")),
        Codec::Xz => Some(("XZ", "xz")),
    };
    if let Some((cc, cf)) = codec_guard {
        g.push_str(&format!(
            "const _: () = assert!(utz::caps::{cc}, \"{name} needs the utz feature `{cf}`\");\n"
        ));
    }
    std::fs::write(format!("{}.guard.rs", out.display()), g)?;
    Ok(())
}
