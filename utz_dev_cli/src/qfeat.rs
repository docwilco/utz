//! The quantized reference features several measurement commands build
//! from source geometry: tzid plus per-polygon rings snapped onto the
//! quantization grid. This is the encoder's quantize prologue rebuilt
//! tool-side as an independent oracle for PIP benches and roundtrip
//! comparisons.

use utz_encode::{Feat, Poly, q_lat, q_lon};

/// A tzid paired with its polygons, each a list of rings of quantized
/// vertices.
pub type QFeat = (String, Vec<Poly<i32>>);

/// Quantizes each feature's rings onto the half-range `qmax` grid,
/// collapsing consecutive duplicates, dropping the duplicated closing
/// vertex, any ring left with fewer than 3 vertices, and any polygon
/// left with no rings, and pairs the result with the tzid.
#[must_use]
pub fn quantize_features(features: &[Feat], qmax: f64) -> Vec<QFeat> {
    features
        .iter()
        .map(|feature| {
            let polys = feature
                .polys
                .iter()
                .filter_map(|poly| {
                    let rings: Poly<i32> = poly
                        .iter()
                        .map(|ring| {
                            let mut quantized: Vec<(i32, i32)> = ring
                                .iter()
                                .map(|&(x, y)| (q_lon(x, qmax), q_lat(y, qmax)))
                                .collect();
                            quantized.dedup();
                            if quantized.first() == quantized.last() && quantized.len() > 1 {
                                quantized.pop();
                            }
                            quantized
                        })
                        .filter(|ring| ring.len() >= 3)
                        .collect();
                    (!rings.is_empty()).then_some(rings)
                })
                .collect();
            (feature.tzid.clone().unwrap_or_default(), polys)
        })
        .collect()
}
