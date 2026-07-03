//! uTZ build + exploration crate.
//!
//! Home of the encoder (topology + RDP + quantization + grid) and the measurement
//! examples ported from the old `formatlab` prototype. Also hosts the viz tool.
//!
//! TODO (per PLAN.md §5): replace the FlatGeobuf `loader` with a GeoJSON parser +
//! conditional-GET downloader so the crate no longer depends on prebuilt `.fgb`.

mod types;
pub use types::*;

pub mod topo;
pub mod encode;
pub mod grid;
pub mod viz;

use std::io::BufReader;

use flatgeobuf::{FallibleStreamingIterator, FeatureProperties, FgbReader};
use geo_types::Geometry;
use geozero::ToGeo;

/// Directory holding the source `.fgb` files during the exploration phase.
/// Override with `UTZ_ASSETS`; defaults to the old spatialtime workspace so the
/// ported measurements keep working until the GeoJSON pipeline lands.
pub fn assets_dir() -> String {
    std::env::var("UTZ_ASSETS").unwrap_or_else(|_| "/home/drwilco/spatialtime/assets".to_string())
}
pub fn fgb_path(ds: &str) -> String {
    format!("{}/timezones_{ds}.fgb", assets_dir())
}

/// Load a dataset (`osm`, `osm1970`, …) into `Feat`s. Temporary FGB-based loader.
pub fn load(ds: &str) -> anyhow::Result<Vec<Feat>> {
    load_path(&fgb_path(ds))
}
pub fn load_path(path: &str) -> anyhow::Result<Vec<Feat>> {
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
