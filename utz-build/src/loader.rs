//! Source loading (PLAN.md §5 steps 1–2). The source is always OSM
//! timezone-boundary-builder **with-oceans**; the only choice is the merge
//! vintage: `now` (65 zones, default) or `1970` (304 zones).
//!
//! Preferred path: download the `GeoJSON` zip (conditional GET) → parse.
//! Legacy path: prebuilt `.fgb` from the old spatialtime workspace (kept until
//! the `GeoJSON` pipeline is the default everywhere).

use std::io::{BufReader, Read as _};
use std::path::Path;

use serde::Deserialize;

use crate::{download, Dataset, Feat, Poly, Ring};

const REPO: &str = "https://github.com/evansiroky/timezone-boundary-builder";

/// Resolve the tag `releases/latest` points at by reading its redirect
/// (`…/releases/tag/<tag>`) — no API, no auth. The tag is cached in
/// `<cache_dir>/tzbb-release.tag` so offline regeneration keeps it;
/// `UTZ_TZBB_RELEASE` pins a tag explicitly (skips the probe). With no
/// network and no cached tag, falls back to `"dev"` with a warning — the
/// zip cache may still serve the data.
///
/// # Errors
/// I/O failure caching the freshly probed tag (probe failures themselves
/// fall back to the cached tag or `"dev"`).
pub fn resolve_release(cache_dir: &Path) -> anyhow::Result<String> {
    if let Ok(tag) = std::env::var("UTZ_TZBB_RELEASE") {
        if !tag.is_empty() {
            return Ok(tag);
        }
    }
    let tag_file = cache_dir.join("tzbb-release.tag");
    let probed: anyhow::Result<String> = (|| {
        let resp = ureq::AgentBuilder::new().redirects(0).build()
            .get(&format!("{REPO}/releases/latest")).call()?;
        resp.header("location")
            .and_then(|l| l.split_once("/releases/tag/"))
            .map(|(_, t)| t.trim_matches('/').to_string())
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow::anyhow!("no /releases/tag/ redirect (status {})", resp.status()))
    })();
    match probed {
        Ok(tag) => {
            std::fs::create_dir_all(cache_dir)?;
            std::fs::write(&tag_file, &tag)?;
            Ok(tag)
        }
        Err(e) => {
            if let Some(tag) = std::fs::read_to_string(&tag_file)
                .ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
            {
                eprintln!("warning: resolving latest TZBB release failed ({e}); using cached tag {tag}");
                return Ok(tag);
            }
            eprintln!("warning: resolving latest TZBB release failed ({e}); tagging container \"dev\"");
            Ok("dev".into())
        }
    }
}

/// TZBB release asset for a dataset, pinned to `release` (so the bytes and
/// the header tag can't skew). TZBB naming: the unsuffixed release is the
/// "Comprehensive" set (`μTZ` `all`); `-1970` = "Same since 1970"; `-now` =
/// "Same since now"; `with-oceans` selects ocean cover.
#[must_use]
pub fn dataset_url(d: Dataset, release: &str) -> String {
    let oceans = if d.oceans { "-with-oceans" } else { "" };
    let vintage = match d.vintage {
        "all" => "",
        v => &format!("-{v}"),
    };
    format!("{REPO}/releases/download/{release}/timezones{oceans}{vintage}.geojson.zip")
}

/// Download (revalidating) + parse a dataset into `Feat`s. Returns the
/// features plus the TZBB release tag they came from.
///
/// # Errors
/// Release-tag caching, download, or zip/`GeoJSON` parse failure.
pub fn load_tzbb(d: Dataset, cache_dir: &Path) -> anyhow::Result<(Vec<Feat>, String)> {
    let release = resolve_release(cache_dir)?;
    let zip_path = download::fetch(&dataset_url(d, &release), cache_dir)?;
    Ok((load_geojson_zip(&zip_path)?, release))
}

/// Parse the first `.json`/`.geojson` entry of a TZBB release zip.
///
/// # Errors
/// I/O or zip-archive failure, no `.json`/`.geojson` entry, or `GeoJSON`
/// parse failure.
pub fn load_geojson_zip(path: &Path) -> anyhow::Result<Vec<Feat>> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))?;
    let name = (0..zip.len())
        .map(|i| zip.name_for_index(i).unwrap_or("").to_string())
        .find(|n| n.ends_with(".json") || n.ends_with(".geojson"))
        .ok_or_else(|| anyhow::anyhow!("no geojson entry in {path:?}"))?;
    let mut buf = Vec::new();
    zip.by_name(&name)?.read_to_end(&mut buf)?;
    parse_geojson(&buf)
}

// Typed mirror of the TZBB FeatureCollection: serde deserializes coordinates
// straight into f64 pairs (no Value DOM — the -now file alone is ~150 MB).
#[derive(Deserialize)]
struct Fc { features: Vec<GjFeature> }
#[derive(Deserialize)]
struct GjFeature { properties: Props, geometry: Geom }
#[derive(Deserialize)]
struct Props { tzid: Option<String> }
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Geom {
    Polygon { coordinates: Vec<Vec<[f64; 2]>> },
    MultiPolygon { coordinates: Vec<Vec<Vec<[f64; 2]>>> },
}

pub fn parse_geojson(bytes: &[u8]) -> anyhow::Result<Vec<Feat>> {
    let fc: Fc = serde_json::from_slice(bytes)?;
    Ok(fc.features.into_iter().map(|f| {
        let polys: Vec<Poly> = match f.geometry {
/// Deserialize a TZBB `GeoJSON` `FeatureCollection` into `Feat`s.
///
/// # Errors
/// JSON that doesn't deserialize as a Polygon/`MultiPolygon` `FeatureCollection`.
            Geom::Polygon { coordinates } => vec![rings_of(coordinates)],
            Geom::MultiPolygon { coordinates } => coordinates.into_iter().map(rings_of).collect(),
        };
        Feat { offset: 0.0, tzid: f.properties.tzid, polys }
    }).collect())
}

fn rings_of(rings: Vec<Vec<[f64; 2]>>) -> Poly {
    rings.into_iter().map(|r| strip_close(r.into_iter().map(|[x, y]| (x, y)).collect())).collect()
}

fn strip_close(mut r: Ring) -> Ring {
    if r.len() > 1 && r.first() == r.last() { r.pop(); }
    r
}
