//! μTZ builder library.
//!
//! Home of the encoder (topology + RDP + quantization + grid + container),
//! source loading, density weighting, and the viz generator. The `utz-build`
//! binary (`gen` plus the measurement and bench subcommands) lives in the
//! `utz-build-cli` crate, which also carries the runtime-reader dependency;
//! this library stays reader-free so build scripts using
//! [`Config`] are not rebuilt by reader-only changes.
//!
//! The source is OSM timezone-boundary-builder. Datasets pick the merge
//! vintage: `now` (64 zones, default), `1970` (304 zones), or `all`
//! (444 zones). Oceans are included by default; a `land-` prefix selects
//! the land-only releases.

#![cfg_attr(docsrs, feature(doc_cfg))]

// encoder core (types, topo, grid, encode) lives in utz-encode (WASM-shared);
// re-export it all so `utz_build::topo::…` paths keep working
pub use utz_encode::*;

pub mod error;
pub use error::{Error, Result};

pub mod config;
pub mod density;
pub mod download;
pub mod loader;
pub mod viz;

pub use config::Config;

use std::path::PathBuf;

/// The two dataset knobs: merge vintage × ocean coverage.
/// TZBB's terminology: `now` = "Same since now", `1970` = "Same since 1970",
/// `all` = "Comprehensive" (every tzid, unsuffixed release). μTZ defaults to
/// with-oceans; a `land-` prefix selects the land-only releases.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dataset {
    /// "now" | "1970" | "all"
    pub vintage: &'static str,
    pub oceans: bool,
}

impl Dataset {
    /// Canonical name: `now`, `1970`, `all`, `land-now`, …
    #[must_use]
    pub fn name(&self) -> String {
        if self.oceans {
            self.vintage.to_string()
        } else {
            format!("land-{}", self.vintage)
        }
    }
    /// Header byte (see encode.rs): bits 0–1 vintage (0=now, 1=1970, 2=all),
    /// bit 2 set = land-only.
    #[must_use]
    pub fn code(&self) -> u8 {
        let v = match self.vintage {
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
    let vintage = match rest {
        "now" | "osm" => "now",
        "1970" | "osm1970" => "1970",
        "all" | "full" | "comprehensive" => "all",
        _ => return Err(Error::UnknownDataset { ds: ds.into() }),
    };
    Ok(Dataset {
        vintage,
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
    loader::load_tzbb(dataset(ds)?, &cache_dir())
}

/// `encode::encode` with population-density-weighted simplification: the
/// per-edge ε multiplier is a simplification knob, so it lives here with the
/// density code; utz-encode itself stays density-agnostic (see the
/// `encode::encode` docs).
///
/// # Errors
/// Payload encoding failure.
pub fn encode_weighted(
    feats: &[Feat],
    p: &encode::Params,
    grid: &density::DensityGrid,
    model: utz_simplify::DensityWeight,
) -> crate::Result<Vec<u8>> {
    let eps_deg = p.eps_m / 111_320.0;
    let algo = utz_encode::encode::to_simplify(p.simplify, eps_deg);
    let t = topo::build_topology_weighted(feats, algo, &|a, b| model.weight(grid.max_along(a, b)));
    Ok(encode::finish(
        &encode::payload_from_topology(&t, &t.arc_coords, feats, p)?.0,
        p.codec,
    )?)
}

/// Workspace-root `cache/` for downloaded TZBB releases (gitignored).
#[must_use]
pub fn cache_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../cache"))
}
