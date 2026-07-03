//! uTZ build + exploration crate.
//!
//! Home of the encoder (topology + RDP + quantization + grid + container) and
//! the measurement examples ported from the old `formatlab` prototype. Also
//! hosts the viz tool.
//!
//! The source is always OSM timezone-boundary-builder **with-oceans** (NED was
//! dropped; see PLAN.md §1). The only dataset choice is the merge vintage:
//! `now` (65 zones, default) or `1970` (304 zones).

mod types;
pub use types::*;

pub mod download;
pub mod encode;
pub mod grid;
pub mod loader;
pub mod topo;
pub mod viz;

use std::io::BufReader;
use std::path::PathBuf;

use flatgeobuf::{FallibleStreamingIterator, FeatureProperties, FgbReader};
use geo_types::Geometry;
use geozero::ToGeo;

/// The two dataset knobs (PLAN.md §6): merge vintage × ocean coverage.
/// TZBB's terminology: `now` = "Same since now", `1970` = "Same since 1970",
/// `all` = "Comprehensive" (every tzid, unsuffixed release). uTZ defaults to
/// with-oceans; a `land-` prefix selects the land-only releases.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Dataset {
    /// "now" | "1970" | "all"
    pub vintage: &'static str,
    pub oceans: bool,
}

impl Dataset {
    /// Canonical name: `now`, `1970`, `all`, `land-now`, …
    pub fn name(&self) -> String {
        if self.oceans { self.vintage.to_string() } else { format!("land-{}", self.vintage) }
    }
    /// Header byte (see encode.rs): bits 0–1 vintage (0=now, 1=1970, 2=all),
    /// bit 2 set = land-only.
    pub fn code(&self) -> u8 {
        let v = match self.vintage { "now" => 0, "1970" => 1, _ => 2 };
        v | if self.oceans { 0 } else { 4 }
    }
}

/// Parse a dataset name (`[land-]now|1970|all`; legacy `osm`/`osm1970` accepted).
pub fn dataset(ds: &str) -> anyhow::Result<Dataset> {
    let (land, rest) = match ds.strip_prefix("land-") {
        Some(r) => (true, r),
        None => (false, ds),
    };
    let vintage = match rest {
        "now" | "osm" => "now",
        "1970" | "osm1970" => "1970",
        "all" | "full" | "comprehensive" => "all",
        _ => anyhow::bail!("unknown dataset {ds:?}: use [land-]now|1970|all"),
    };
    Ok(Dataset { vintage, oceans: !land })
}

/// Load a dataset. `UTZ_SOURCE=tzbb` forces the download+GeoJSON pipeline,
/// `UTZ_SOURCE=fgb` forces the legacy prebuilt `.fgb`; default prefers the
/// `.fgb` when it exists (no network, with-oceans now/1970 only) and falls
/// back to downloading.
pub fn load(ds: &str) -> anyhow::Result<Vec<Feat>> {
    let d = dataset(ds)?;
    let fgb = fgb_path(&d);
    match std::env::var("UTZ_SOURCE").as_deref() {
        Ok("fgb") => load_fgb(&fgb.ok_or_else(|| anyhow::anyhow!("no legacy .fgb for dataset {}", d.name()))?),
        Ok("tzbb") => loader::load_tzbb(d, &cache_dir()),
        _ => match fgb {
            Some(p) if std::path::Path::new(&p).exists() => load_fgb(&p),
            _ => loader::load_tzbb(d, &cache_dir()),
        },
    }
}

/// Workspace-root `cache/` for downloaded TZBB releases (gitignored).
pub fn cache_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../cache"))
}

/// Legacy prebuilt `.fgb` location (old spatialtime workspace; with-oceans
/// now/1970 only). Override the directory with `UTZ_ASSETS`.
pub fn fgb_path(d: &Dataset) -> Option<String> {
    if !d.oceans {
        return None;
    }
    let dir = std::env::var("UTZ_ASSETS")
        .unwrap_or_else(|_| "/home/drwilco/spatialtime/assets".to_string());
    match d.vintage {
        "now" => Some(format!("{dir}/timezones_osm.fgb")),
        "1970" => Some(format!("{dir}/timezones_osm1970.fgb")),
        _ => None,
    }
}

fn load_fgb(path: &str) -> anyhow::Result<Vec<Feat>> {
    let bytes = std::fs::read(path)?;
    let mut reader = BufReader::new(&bytes[..]);
    let fgb = FgbReader::open(&mut reader)?;
    let mut seq = fgb.select_all_seq()?;
    let mut feats = Vec::new();
    while let Some(f) = seq.next()? {
        let props = f.properties()?;
        let offset: f64 = props.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let tzid = props.get("tzid").filter(|s| !s.is_empty()).cloned();
        let mut polys: Vec<Poly> = Vec::new();
        if let Ok(Geometry::MultiPolygon(mp)) = f.to_geo() {
            for p in mp {
                let mut poly: Poly = vec![strip_close(p.exterior().coords().map(|c| (c.x, c.y)).collect())];
                for r in p.interiors() { poly.push(strip_close(r.coords().map(|c| (c.x, c.y)).collect())); }
                polys.push(poly);
            }
        }
        feats.push(Feat { offset, tzid, polys });
    }
    Ok(feats)
}

fn strip_close(mut r: Ring) -> Ring {
    if r.len() > 1 && r.first() == r.last() { r.pop(); }
    r
}
