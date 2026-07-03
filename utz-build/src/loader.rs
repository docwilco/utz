//! Source loading (PLAN.md §5 steps 1–2). The source is always OSM
//! timezone-boundary-builder **with-oceans**; the only choice is the merge
//! vintage: `now` (65 zones, default) or `1970` (304 zones).
//!
//! Preferred path: download the GeoJSON zip (conditional GET) → parse.
//! Legacy path: prebuilt `.fgb` from the old spatialtime workspace (kept until
//! the GeoJSON pipeline is the default everywhere).

use std::io::{BufReader, Read as _};
use std::path::Path;

use serde::Deserialize;

use crate::{download, Dataset, Feat, Poly, Ring};

/// TZBB release asset for a dataset (`releases/latest`). TZBB naming: the
/// unsuffixed release is the "Comprehensive" set (μTZ `all`); `-1970` = "Same
/// since 1970"; `-now` = "Same since now"; `with-oceans` selects ocean cover.
pub fn dataset_url(d: Dataset) -> String {
    let base = "https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download";
    let oceans = if d.oceans { "-with-oceans" } else { "" };
    let vintage = match d.vintage {
        "all" => "",
        v => &format!("-{v}"),
    };
    format!("{base}/timezones{oceans}{vintage}.geojson.zip")
}

/// Download (revalidating) + parse a dataset into `Feat`s.
pub fn load_tzbb(d: Dataset, cache_dir: &Path) -> anyhow::Result<Vec<Feat>> {
    let zip_path = download::fetch(&dataset_url(d), cache_dir)?;
    load_geojson_zip(&zip_path)
}

/// Parse the first `.json`/`.geojson` entry of a TZBB release zip.
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
