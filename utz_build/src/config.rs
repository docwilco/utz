//! The consumer-facing builder API for the `custom` tier, where the typed
//! config IS the build config (rustdoc'd, IDE-completable, with no file
//! discovery). It is meant for a consumer `build.rs` with `utz_build` as
//! a build-dependency (the `prost-build` pattern):
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
//! into the cache, never committed; downloads are conditional-GET-cached
//! so regeneration is cheap.
//!
//! The preset recipes double as constructors. Start from one and
//! override a single knob instead of spelling the whole recipe:
//! `Config::compact().codec(Codec::Uncompressed)`. The recipe values
//! themselves live in one place, the [`utz_common::presets`] table; the
//! constructors here are thin wrappers over the `From<&Recipe>`
//! conversion.

use std::path::PathBuf;

use utz_common::presets::{self, Recipe};
use utz_encode::encode::{self, Codec, GeomEncoding, Params, SimplifyAlgorithm};

/// The builder for a custom `.utz` asset. The defaults are dataset `now`,
/// RDP ε=500 m, no density weighting, i24, a 2° grid, gzip, varint-arcs
/// geometry, and output to `$OUT_DIR/tz.utz`.
#[derive(Clone, Debug)]
pub struct Config {
    dataset: String,
    epsilon_m: f64,
    quant_bits: u32,
    grid_deg: f64,
    codec: Codec,
    simplify: SimplifyAlgorithm,
    geom: GeomEncoding,
    density_weight_floor: Option<f64>,
    out: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dataset: "now".into(),
            epsilon_m: 500.0,
            quant_bits: 24,
            grid_deg: 2.0,
            codec: Codec::Gzip,
            simplify: SimplifyAlgorithm::Rdp,
            geom: GeomEncoding::VarintArcs,
            density_weight_floor: None,
            out: None,
            cache_dir: None,
        }
    }
}

impl Config {
    /// Returns a config at the documented defaults; chain knob methods to
    /// override them, then call [`generate()`](Config::generate).
    #[must_use]
    pub fn new() -> Self {
        Config::default()
    }

    /// Returns the `tiny` preset recipe ([`presets::TINY`]): RDP
    /// ε=10 000 m with pop-density floor 0.001, i16, a 2° grid, and
    /// gzip. A preset constructor is a starting point for one-knob
    /// variants: `Config::tiny().geom(GeomEncoding::Coarse)`.
    #[must_use]
    pub fn tiny() -> Self {
        Config::from(&presets::TINY)
    }

    /// Returns the `tiny-static` preset recipe ([`presets::TINY_STATIC`]),
    /// which is `tiny` stored uncompressed so the asset is readable in
    /// place with zero decode RAM.
    #[must_use]
    pub fn tiny_static() -> Self {
        Config::from(&presets::TINY_STATIC)
    }

    /// Returns the `compact` preset recipe ([`presets::COMPACT`]): RDP
    /// ε=1 000 m with pop-density floor 0.001, i24, a 4/3° grid, and xz.
    #[must_use]
    pub fn compact() -> Self {
        Config::from(&presets::COMPACT)
    }

    /// Returns the `balanced` preset recipe ([`presets::BALANCED`]): RDP
    /// ε=50 m with pop-density floor 0.020, i24, a 2/3° grid, and
    /// brotli.
    #[must_use]
    pub fn balanced() -> Self {
        Config::from(&presets::BALANCED)
    }

    /// Returns the `accurate` preset recipe ([`presets::ACCURATE`]):
    /// dataset `all` (the full Comprehensive zone set; the other presets
    /// use `now`), RDP ε=10 m with pop-density floor 0.10, i32, a 0.5°
    /// grid, and brotli.
    #[must_use]
    pub fn accurate() -> Self {
        Config::from(&presets::ACCURATE)
    }

    /// Sets the dataset: `[land-]now|1970|all` (see
    /// [`Dataset`](crate::Dataset)).
    #[must_use]
    pub fn dataset(mut self, dataset: &str) -> Self {
        self.dataset = dataset.into();
        self
    }

    /// Sets the simplification tolerance ceiling in meters (default 500).
    /// The value is the ε of whichever
    /// [`simplify_algorithm()`](Config::simplify_algorithm) runs: it is a
    /// max-deviation bound for RDP and Imai–Iri, and Visvalingam derives
    /// its area threshold as ε².
    #[must_use]
    pub fn rdp_meters(mut self, epsilon_m: f64) -> Self {
        self.epsilon_m = epsilon_m;
        self
    }

    /// Sets the quantization width: 16 / 24 / 32 (default 24). Any other
    /// value fails at [`generate()`](Config::generate), not here.
    #[must_use]
    pub fn quant_bits(mut self, bits: u32) -> Self {
        self.quant_bits = bits;
        self
    }

    /// Sets the grid cell size in degrees, 0.1–45 (default 2). Fractional
    /// sizes are fine (the presets use 4/3, 2/3, and 0.5); out-of-range
    /// values fail at [`generate()`](Config::generate).
    #[must_use]
    pub fn grid_deg(mut self, degrees: f64) -> Self {
        self.grid_deg = degrees;
        self
    }

    /// Sets the payload codec. `Codec::Uncompressed` gives a `core`-rung
    /// asset: zero decode RAM at the cost of more flash.
    #[must_use]
    pub fn codec(mut self, codec: Codec) -> Self {
        self.codec = codec;
        self
    }

    /// Sets the simplification algorithm, default [`SimplifyAlgorithm::Rdp`].
    /// [`SimplifyAlgorithm::ImaiIri`] gives provably minimum vertices for the
    /// same ε (−4 to −19% measured, at a slower encode);
    /// [`SimplifyAlgorithm::Visvalingam`] trades the deviation bound for a
    /// cartographically smoother caricature; [`SimplifyAlgorithm::None`] keeps
    /// every source vertex.
    #[must_use]
    pub fn simplify_algorithm(mut self, algorithm: SimplifyAlgorithm) -> Self {
        self.simplify = algorithm;
        self
    }

    /// Sets the geometry encoding. The default is
    /// [`GeomEncoding::VarintArcs`] (smallest flash); the measured
    /// size/speed ladder is the table on [`GeomEncoding`].
    #[must_use]
    pub fn geom(mut self, geom: GeomEncoding) -> Self {
        self.geom = geom;
        self
    }

    /// Enables population-density-weighted simplification. The value is
    /// the ε multiplier floor in the densest cells, strictly between 0
    /// and 1 (the presets use 0.001–0.10; the default is off, uniform ε;
    /// out-of-range values fail at [`generate()`](Config::generate)).
    /// First use downloads GHS-POP (~460 MB, cached).
    #[must_use]
    pub fn density_weight_floor(mut self, floor: f64) -> Self {
        self.density_weight_floor = Some(floor);
        self
    }

    /// Sets where to write the asset. The default is `$OUT_DIR/tz.utz`
    /// (the build.rs context).
    #[must_use]
    pub fn out_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.out = Some(path.into());
        self
    }

    /// Sets where source downloads are cached. The default is the
    /// [`cache_dir()`](crate::cache_dir) chain (`UTZ_CACHE_DIR`, then the
    /// per-user cache directory). Point this at a pre-fetched directory
    /// for hermetic builds with no network. Build scripts relying on the
    /// environment default should emit
    /// `cargo:rerun-if-env-changed=UTZ_CACHE_DIR` (and
    /// `UTZ_TZBB_RELEASE` if unpinned).
    #[must_use]
    pub fn cache_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(path.into());
        self
    }

    /// Fetches the sources (cached), builds the asset, writes it, and
    /// returns the path. It also writes `<out>.guard.rs`, a compile-time
    /// assertion of the `utz` features this asset needs (via `utz::caps`);
    /// `include!` it next to the `include_bytes!` so a feature mismatch
    /// fails the build instead of the first load.
    ///
    /// # Errors
    /// Returns an error for an invalid dataset name, out-of-range
    /// `quant_bits`/`grid_deg`, a source download/parse failure, an
    /// encoding failure, a missing `OUT_DIR` when no `out_path` is set,
    /// or an I/O failure writing the asset and its guard file.
    pub fn generate(self) -> crate::Result<PathBuf> {
        let cache = self.cache_dir.clone().unwrap_or_else(crate::cache_dir);
        let (features, release) = crate::load_with_release_in(&self.dataset, &cache)?;
        let params = Params {
            dataset: crate::dataset(&self.dataset)?.code(),
            tzbb_release: &release,
            epsilon_m: self.epsilon_m,
            quant_bits: self.quant_bits,
            grid_deg: self.grid_deg,
            codec: self.codec,
            simplify: self.simplify,
            geom: self.geom,
            density_weight_floor: self.density_weight_floor,
        };
        let bytes = match self.density_weight_floor {
            Some(floor) => {
                let grid = crate::density::DensityGrid::load(&cache)?;
                crate::encode_weighted(
                    &features,
                    &params,
                    &grid,
                    utz_simplify::DensityWeight::new(floor),
                )?
            }
            None => encode::encode(&features, &params)?,
        };
        let out = if let Some(path) = self.out {
            path
        } else {
            let dir = std::env::var_os("OUT_DIR").ok_or_else(|| crate::Error::NoOutDir)?;
            PathBuf::from(dir).join("tz.utz")
        };
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, &bytes)?;
        write_guard(&out, self.geom, self.codec)?;
        Ok(out)
    }
}

/// Converts a recipe, typically one of the [`utz_common::presets`]
/// constants, into a config carrying its knobs. The named preset
/// constructors are thin wrappers over this; use it directly to drive
/// the builder from the table (e.g. iterating [`presets::ALL`]).
impl From<&Recipe> for Config {
    fn from(recipe: &Recipe) -> Config {
        let config = Config::new()
            .dataset(recipe.dataset.name())
            .rdp_meters(recipe.epsilon_m)
            .quant_bits(recipe.quant_bits.bits())
            .grid_deg(recipe.grid_deg)
            .codec(recipe.codec)
            .simplify_algorithm(recipe.simplify_algorithm)
            .geom(recipe.geom);
        match recipe.density_weight_floor() {
            Some(floor) => config.density_weight_floor(floor),
            None => config,
        }
    }
}

/// Converts an owned recipe into a config; see [`From<&Recipe>`](#impl-From<%26Recipe>-for-Config).
impl From<Recipe> for Config {
    fn from(recipe: Recipe) -> Config {
        Config::from(&recipe)
    }
}

/// Emits `<asset>.guard.rs`, the `const _` assertions against `utz::caps`
/// for the geometry decoder and codec the asset needs (shared by
/// [`Config::generate()`] and the `gen` CLI).
///
/// # Errors
/// Returns an error on an I/O failure writing `<out>.guard.rs`.
pub fn write_guard(out: &std::path::Path, geom: GeomEncoding, codec: Codec) -> crate::Result<()> {
    let name = out
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or("asset");
    let (geom_cap_name, geom_feature_name) = match geom {
        GeomEncoding::VarintArcs => ("GEOM_VARINT_ARCS", "geom-varint-arcs"),
        GeomEncoding::FixedWidthArcs => ("GEOM_FIXED_WIDTH_ARCS", "geom-fixed-width-arcs"),
        GeomEncoding::FullRings => ("GEOM_FULL_RINGS", "geom-full-rings"),
        GeomEncoding::Coarse => ("GEOM_COARSE", "geom-coarse"),
    };
    let mut guard_source = format!(
        "// generated by utz_build alongside {name}: include! this next to the\n\
         // include_bytes! to turn a missing-feature load error into a compile error.\n\
         const _: () = assert!(utz::caps::{geom_cap_name}, \"{name} needs the utz feature `{geom_feature_name}`\");\n"
    );
    let codec_guard = match codec {
        Codec::Uncompressed => None,
        Codec::Gzip => Some(("CODEC_GZIP", "gzip")),
        Codec::Zstd => Some(("CODEC_ZSTD", "ruzstd` or `zstd-sys")),
        Codec::Brotli => Some(("CODEC_BROTLI", "brotli")),
        Codec::Xz => Some(("CODEC_XZ", "xz")),
    };
    if let Some((codec_cap_name, codec_feature_name)) = codec_guard {
        use std::fmt::Write as _;
        let _ = writeln!(
            guard_source,
            "const _: () = assert!(utz::caps::{codec_cap_name}, \"{name} needs the utz feature `{codec_feature_name}`\");"
        );
    }
    std::fs::write(format!("{}.guard.rs", out.display()), guard_source)?;
    Ok(())
}
