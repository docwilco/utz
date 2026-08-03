//! μTZ builder library: generate custom `.utz` assets from a `build.rs`.
//!
//! Everything routes through the typed builder, [`Config`]. In a
//! `build.rs` (with `utz-build` as a build-dependency):
//!
//! ```no_run
//! utz_build::Config::new()
//!     .dataset("now")     // [land-]now | 1970 | all
//!     .rdp_meters(500.0)  // simplification tolerance ceiling
//!     .quant_bits(24)     // 16 / 24 / 32
//!     .codec(utz_build::Codec::Gzip)
//!     .generate()?;       // writes $OUT_DIR/tz.utz (+ .guard.rs)
//! # Ok::<(), utz_build::Error>(())
//! ```
//!
//! The [utz crate's "Building a custom asset"][custom] section is the
//! full walkthrough: embedding the asset, the guard file, and matching
//! reader features. Every knob is a [`Config`] method; everything else
//! this crate exposes (source loading, density weighting) is machinery
//! those methods drive.
//!
//! # Source data
//!
//! Sources download on first use into a per-user cache directory
//! (`$XDG_CACHE_HOME/utz-build`, overridable with `UTZ_CACHE_DIR` or
//! [`Config::cache_dir()`](config::Config::cache_dir); see
//! [`cache_dir()`]) and are revalidated with conditional GETs: the
//! [timezone-boundary-builder] `GeoJSON` (tens of MB per dataset,
//! [ODbL]), and for density-weighted recipes the [GHS-POP] population
//! raster (~460 MB once, [CC BY 4.0]). Set `UTZ_TZBB_RELEASE` to pin a
//! TZBB release for reproducible builds (see [`loader`]).
//!
//! [custom]: ../utz/index.html#building-a-custom-asset
//! [ODbL]: https://opendatacommons.org/licenses/odbl/
//! [GHS-POP]: https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php
//! [CC BY 4.0]: https://creativecommons.org/licenses/by/4.0/
//!
//! # Datasets
//!
//! The source data is OSM [timezone-boundary-builder]. The dataset
//! picks which TZBB release an asset is generated from and is baked
//! in at generation time. Its two knobs are the zone set and ocean
//! coverage: `now` and `1970` merge zones whose rules are identical
//! since that date, `all` keeps every distinct tzid, and a `land-`
//! prefix selects the land-only releases (oceans are covered by
//! default).
//!
//! | zone set | zones | `land-` zones | merge |
//! |----------|------:|--------------:|-------|
//! | `now`    |    64 |            63 | zones identical from today onward merged |
//! | `1970`   |   304 |           301 | zones identical since 1970 merged |
//! | `all`    |   444 |           419 | every distinct tzid kept |
//!
//! Dropping the oceans does not shrink an asset: the shared-arc
//! geometry encodings make ocean coverage nearly free, and the
//! `land-` variants actually measure slightly bigger (at the tiny
//! recipe's knobs, uncompressed: `land-now` 89.7 KiB vs `now`
//! 86.7 KiB). Their best use is coarse (grid-only) assets that will
//! only ever be queried for land coordinates: a coarse asset answers
//! at cell precision, and with oceans covered a land point near the
//! coast resolves to the ocean timezone whenever most of its cell is
//! ocean; without ocean zones the cell keeps the land answer.
//!
//! # Related crates
//!
//! The [`utz-build-cli`][cli] binary (`gen` and `gen-preset`, the CLI
//! counterpart of a `build.rs`) lives in its own crate: it carries the
//! runtime-reader dependency, so this library stays reader-free and
//! build scripts are not rebuilt by reader-only changes. The generated
//! assets are read by [`utz`][reader]; the repo-internal measurement
//! and viewer tooling lives in the unpublished `utz-dev-cli` and
//! `utz-viz` crates.
//!
//! [timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
//! [`Config`]: ../utz_build/config/struct.Config.html
//! [cli]: ../utz_build_cli/index.html
//! [`loader`]: ../utz_build/loader/index.html
//! [reader]: ../utz/index.html

#![cfg_attr(docsrs, feature(doc_cfg))]

// The consumer surface: Config plus the names its knobs and results
// need. The encoder machinery itself stays in utz-encode; the internal
// tooling (utz-dev-cli, utz-viz) depends on that crate directly.
pub use utz_encode::encode::{Codec, GeomEncoding, SimplifyAlgo};
pub use utz_encode::Feat;

pub mod error;
pub use error::{Error, Result};

pub mod config;
pub mod density;
pub mod download;
pub mod loader;

pub use config::Config;

use std::path::PathBuf;

/// The two dataset knobs: zone set × ocean coverage.
/// TZBB's terminology: `now` = "Same since now", `1970` = "Same since 1970",
/// `all` = "Comprehensive" (every tzid, unsuffixed release). μTZ defaults to
/// with-oceans; a `land-` prefix selects the land-only releases.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dataset {
    /// `"now"` | `"1970"` | `"all"`: how aggressively zones with
    /// identical rules are merged.
    pub zone_set: &'static str,
    /// Whether maritime timezones cover the oceans (`false` = the
    /// `land-` releases).
    pub oceans: bool,
}

impl Dataset {
    /// Canonical name: `now`, `1970`, `all`, `land-now`, …
    #[must_use]
    pub fn name(&self) -> String {
        if self.oceans {
            self.zone_set.to_string()
        } else {
            format!("land-{}", self.zone_set)
        }
    }
    /// Header byte (see encode.rs): bits 0–1 zone set (0=now, 1=1970, 2=all),
    /// bit 2 set = land-only.
    #[must_use]
    pub fn code(&self) -> u8 {
        let v = match self.zone_set {
            "now" => 0,
            "1970" => 1,
            _ => 2,
        };
        v | if self.oceans { 0 } else { 4 }
    }
}

/// Parse a dataset name (`[land-]now|1970|all`; legacy `osm`/`osm1970` accepted).
///
/// # Errors
/// Unrecognized dataset name.
pub fn dataset(ds: &str) -> crate::Result<Dataset> {
    let (land, rest) = match ds.strip_prefix("land-") {
        Some(r) => (true, r),
        None => (false, ds),
    };
    let zone_set = match rest {
        "now" | "osm" => "now",
        "1970" | "osm1970" => "1970",
        "all" | "full" | "comprehensive" => "all",
        _ => return Err(Error::UnknownDataset { ds: ds.into() }),
    };
    Ok(Dataset {
        zone_set,
        oceans: !land,
    })
}

/// Load a dataset via the download+`GeoJSON` pipeline (conditional-GET
/// cached).
///
/// # Errors
/// See [`load_with_release`].
pub fn load(ds: &str) -> crate::Result<Vec<Feat>> {
    Ok(load_with_release(ds)?.0)
}

/// [`load`] plus the TZBB release tag the features came from, for stamping
/// container headers (provenance). `"dev"` when the source isn't a
/// pinned release (offline fallback).
///
/// # Errors
/// Invalid dataset name, or TZBB download/parse failure.
pub fn load_with_release(ds: &str) -> crate::Result<(Vec<Feat>, String)> {
    load_with_release_in(ds, &cache_dir())
}

/// [`load_with_release`] against an explicit cache directory instead of
/// the [`cache_dir()`] chain.
///
/// # Errors
/// As [`load_with_release`].
pub fn load_with_release_in(
    ds: &str,
    cache_dir: &std::path::Path,
) -> crate::Result<(Vec<Feat>, String)> {
    loader::load_tzbb(dataset(ds)?, cache_dir)
}

/// `encode::encode()` with population-density-weighted simplification: the
/// per-edge ε multiplier is a simplification knob, so it lives here with the
/// density code; utz-encode itself stays density-agnostic (see the
/// `encode::encode()` docs).
///
/// # Errors
/// Payload encoding failure.
pub fn encode_weighted(
    feats: &[Feat],
    p: &utz_encode::encode::Params,
    grid: &density::DensityGrid,
    model: utz_simplify::DensityWeight,
) -> crate::Result<Vec<u8>> {
    let eps_deg = p.eps_m / 111_320.0;
    let algo = utz_encode::encode::to_simplify(p.simplify, eps_deg);
    let t = utz_encode::topo::build_topology_weighted(feats, algo, &|a, b| {
        model.weight(grid.max_along(a, b))
    });
    Ok(utz_encode::encode::finish(
        &utz_encode::encode::payload_from_topology(&t, &t.arc_coords, feats, p)?.0,
        p.codec,
    )?)
}

/// The source-data cache directory, resolved at call time:
///
/// 1. `$UTZ_CACHE_DIR` (the μTZ workspace pins this to its `cache/`
///    via `.cargo/config.toml`, so in-repo builds share one cache)
/// 2. `$XDG_CACHE_HOME/utz-build`
/// 3. `%LOCALAPPDATA%\utz-build` (Windows) or `$HOME/.cache/utz-build`
/// 4. `$OUT_DIR/utz-build-cache` (build-script last resort)
///
/// [`Config::cache_dir()`](config::Config::cache_dir) overrides the
/// chain per build. Everything inside is a re-fetchable download (TZBB
/// zips keyed per release, the ~460 MB GHS-POP raster and its decoded
/// sidecar): safe to delete. Build scripts that depend on the chain
/// should emit `cargo:rerun-if-env-changed=UTZ_CACHE_DIR`.
#[must_use]
pub fn cache_dir() -> PathBuf {
    use std::path::Path;
    if let Some(p) = std::env::var_os("UTZ_CACHE_DIR").filter(|p| !p.is_empty()) {
        return p.into();
    }
    if let Some(p) = std::env::var_os("XDG_CACHE_HOME").filter(|p| !p.is_empty()) {
        return Path::new(&p).join("utz-build");
    }
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty()) {
        return Path::new(&p).join("utz-build");
    }
    if let Some(p) = std::env::var_os("HOME").filter(|p| !p.is_empty()) {
        return Path::new(&p).join(".cache").join("utz-build");
    }
    if let Some(p) = std::env::var_os("OUT_DIR").filter(|p| !p.is_empty()) {
        return Path::new(&p).join("utz-build-cache");
    }
    PathBuf::from("utz-build-cache")
}
