//! This module loads the source data. The source is OSM
//! timezone-boundary-builder; datasets pick ocean cover (with-oceans or
//! land-only) and the zone set: `now` (64 zones, default), `1970`
//! (304 zones), or `all` (444 zones).
//!
//! The flow downloads the `GeoJSON` zip (with a conditional GET) and
//! parses it.

use std::io::{BufReader, Read as _};
use std::path::Path;

use serde::Deserialize;

use crate::{Dataset, Error, Feat, download};
use utz_encode::{Poly, Ring};

const REPO: &str = "https://github.com/evansiroky/timezone-boundary-builder";

/// Resolves the tag `releases/latest` points at by reading its redirect
/// (`…/releases/tag/<tag>`), so no API and no auth are needed. The tag is
/// cached in `<cache_dir>/tzbb-release.tag` so offline regeneration keeps
/// it, and `UTZ_TZBB_RELEASE` pins a tag explicitly (skipping the probe).
/// With no network and no cached tag, the function falls back to `"dev"`
/// with a warning; the per-release zip cache (`tzbb/<release>/`) will
/// then be empty, so a first-ever offline run fails at download rather
/// than serving mislabeled bytes.
///
/// # Errors
/// Returns an error only for an I/O failure while caching the freshly
/// probed tag (probe failures themselves fall back to the cached tag or
/// `"dev"`).
pub fn resolve_release(cache_dir: &Path) -> crate::Result<String> {
    if let Ok(tag) = std::env::var("UTZ_TZBB_RELEASE")
        && !tag.is_empty()
    {
        return Ok(tag);
    }
    let tag_file = cache_dir.join("tzbb-release.tag");
    let probed: crate::Result<String> = (|| {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .build()
            .into();
        let response = agent.get(format!("{REPO}/releases/latest")).call()?;
        response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .and_then(|location| location.split_once("/releases/tag/"))
            .map(|(_, tag)| tag.trim_matches('/').to_string())
            .filter(|tag| !tag.is_empty())
            .ok_or_else(|| Error::NoReleaseRedirect {
                status: response.status().as_u16(),
            })
    })();
    match probed {
        Ok(tag) => {
            std::fs::create_dir_all(cache_dir)?;
            std::fs::write(&tag_file, &tag)?;
            Ok(tag)
        }
        Err(error) => {
            if let Some(tag) = std::fs::read_to_string(&tag_file)
                .ok()
                .map(|contents| contents.trim().to_string())
                .filter(|tag| !tag.is_empty())
            {
                eprintln!(
                    "warning: resolving latest TZBB release failed ({error}); using cached tag {tag}"
                );
                return Ok(tag);
            }
            eprintln!(
                "warning: resolving latest TZBB release failed ({error}); tagging asset \"dev\""
            );
            Ok("dev".into())
        }
    }
}

/// Builds the URL of the TZBB release asset for a dataset, pinned to
/// `release` (so the bytes and the header tag can't skew). In TZBB
/// naming, the unsuffixed release is the "Comprehensive" set (μTZ `all`),
/// the `-1970` suffix is "Same since 1970", `-now` is "Same since now",
/// and `with-oceans` selects ocean cover.
#[must_use]
pub fn dataset_url(dataset: Dataset, release: &str) -> String {
    let oceans = if dataset.oceans() { "-with-oceans" } else { "" };
    let zone_set = match dataset.zone_set() {
        "all" => "",
        set_name => &format!("-{set_name}"),
    };
    format!("{REPO}/releases/download/{release}/timezones{oceans}{zone_set}.geojson.zip")
}

/// Downloads (revalidating) and parses a dataset into `Feat`s, returning
/// the features plus the TZBB release tag they came from.
///
/// # Errors
/// Returns any release-tag caching, download, or zip/`GeoJSON` parse
/// failure.
pub fn load_tzbb(dataset: Dataset, cache_dir: &Path) -> crate::Result<(Vec<Feat>, String)> {
    let release = resolve_release(cache_dir)?;
    // key the download by release: TZBB asset basenames are
    // release-independent, and a flat cache would serve one release's
    // bytes under another's tag when offline
    let release_dir = cache_dir.join("tzbb").join(&release);
    let zip_path = download::fetch(&dataset_url(dataset, &release), &release_dir)?;
    Ok((load_geojson_zip(&zip_path)?, release))
}

/// Parses the first `.json`/`.geojson` entry of a TZBB release zip.
///
/// # Errors
/// Returns an I/O or zip-archive failure, an error when the zip has no
/// `.json`/`.geojson` entry, or a `GeoJSON` parse failure.
pub fn load_geojson_zip(path: &Path) -> crate::Result<Vec<Feat>> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))?;
    let name = (0..zip.len())
        .map(|i| zip.name_for_index(i).unwrap_or("").to_string())
        .find(|entry_name| {
            Path::new(entry_name).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("geojson")
            })
        })
        .ok_or_else(|| Error::NoGeojsonEntry { path: path.into() })?;
    let mut bytes = Vec::new();
    zip.by_name(&name)?.read_to_end(&mut bytes)?;
    parse_geojson(&bytes)
}

// Typed mirror of the TZBB FeatureCollection: serde deserializes coordinates
// straight into f64 pairs (no Value DOM — the -now file alone is ~150 MB).
#[derive(Deserialize)]
struct Fc {
    features: Vec<GjFeature>,
}
#[derive(Deserialize)]
struct GjFeature {
    properties: Props,
    geometry: Geom,
}
#[derive(Deserialize)]
struct Props {
    tzid: Option<String>,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Geom {
    Polygon {
        coordinates: Vec<Vec<[f64; 2]>>,
    },
    MultiPolygon {
        coordinates: Vec<Vec<Vec<[f64; 2]>>>,
    },
}

/// Deserializes a TZBB `GeoJSON` `FeatureCollection` into `Feat`s.
///
/// # Errors
/// Returns an error for JSON that doesn't deserialize as a
/// Polygon/`MultiPolygon` `FeatureCollection`.
pub fn parse_geojson(bytes: &[u8]) -> crate::Result<Vec<Feat>> {
    let collection: Fc = serde_json::from_slice(bytes)?;
    Ok(collection
        .features
        .into_iter()
        .map(|feature| {
            let polys: Vec<Poly> = match feature.geometry {
                Geom::Polygon { coordinates } => vec![rings_of(coordinates)],
                Geom::MultiPolygon { coordinates } => {
                    coordinates.into_iter().map(rings_of).collect()
                }
            };
            Feat {
                offset: 0.0,
                tzid: feature.properties.tzid,
                polys,
            }
        })
        .collect())
}

fn rings_of(rings: Vec<Vec<[f64; 2]>>) -> Poly {
    rings
        .into_iter()
        .map(|ring| strip_close(ring.into_iter().map(|[x, y]| (x, y)).collect()))
        .collect()
}

fn strip_close(mut ring: Ring) -> Ring {
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    ring
}
