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

use std::path::PathBuf;

use crate::encode::{self, Codec, Params};

/// Builder for a custom `.utz` asset. Defaults: dataset `now`, RDP ε=500 m,
/// i24, 2° grid, gzip.
#[derive(Clone, Debug)]
pub struct Config {
    dataset: String,
    eps_m: f64,
    quant_bits: u32,
    grid_deg: f64,
    codec: Codec,
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
            density_weight_floor: None,
            out: None,
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Config::default()
    }

    /// Dataset: `[land-]now|1970|all` (§6).
    pub fn dataset(mut self, ds: &str) -> Self {
        self.dataset = ds.into();
        self
    }

    /// Simplification tolerance ceiling in meters (RDP ε).
    pub fn rdp_meters(mut self, eps_m: f64) -> Self {
        self.eps_m = eps_m;
        self
    }

    /// Quantization width: 16 / 24 / 32 (§8).
    pub fn quant_bits(mut self, bits: u32) -> Self {
        self.quant_bits = bits;
        self
    }

    /// Grid cell size in degrees, 0.1–45 (§10).
    pub fn grid_deg(mut self, deg: f64) -> Self {
        self.grid_deg = deg;
        self
    }

    /// Payload codec (§7). `Codec::Uncompressed` gives a `core`-rung asset:
    /// zero decode RAM, more flash.
    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Population-density-weighted simplification: ε multiplier floor in the
    /// densest cells (tiny uses 1e-3). First use downloads GHS-POP (~460 MB,
    /// cached).
    pub fn density_weight_floor(mut self, w_min: f64) -> Self {
        self.density_weight_floor = Some(w_min);
        self
    }

    /// Where to write the asset. Default: `$OUT_DIR/tz.utz` (build.rs context).
    pub fn out_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.out = Some(p.into());
        self
    }

    /// Fetch sources (cached), build the container, write it, return the path.
    pub fn generate(self) -> anyhow::Result<PathBuf> {
        let feats = crate::load(&self.dataset)?;
        let p = Params {
            dataset: crate::dataset(&self.dataset)?.code(),
            tzbb_release: "dev", // TODO: thread the real release tag through loader
            eps_m: self.eps_m,
            quant_bits: self.quant_bits,
            grid_deg: self.grid_deg,
            codec: self.codec,
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
        let out = match self.out {
            Some(p) => p,
            None => {
                let dir = std::env::var_os("OUT_DIR")
                    .ok_or_else(|| anyhow::anyhow!("no OUT_DIR (not in a build.rs?) — set .out_path()"))?;
                PathBuf::from(dir).join("tz.utz")
            }
        };
        std::fs::write(&out, &bytes)?;
        Ok(out)
    }
}
