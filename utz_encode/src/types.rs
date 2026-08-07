//! The shared geometry types and quantization helpers for the encoder and
//! the measurement tools.

// Coordinate tuples are (lon, lat) — equivalently (x, y) with x = lon —
// everywhere in the workspace: builder f64 degrees and quantized i32 grid
// units alike. The aliases carry the semantic split: a Ring is closed
// (implicitly; no duplicated closing vertex), an Arc is an open polyline
// shared between the rings that reference it.
/// One closed ring. The closing vertex is not duplicated.
pub type Ring<T = f64> = Vec<(T, T)>;
/// One polygon's rings, the exterior ring first and the interior (hole)
/// rings after.
pub type Poly<T = f64> = Vec<Ring<T>>;
/// One open shared-boundary polyline (see `topo`). This is NOT
/// `std::sync::Arc`.
pub type Arc<T = f64> = Vec<(T, T)>;

/// One timezone feature as loaded from the source `GeoJSON`: it carries the
/// polygons plus the tzid and UTC offset metadata.
pub struct Feat {
    /// The UTC offset in hours (fractional values allowed), which serves
    /// ocean zones without a tzid.
    pub offset: f64,
    /// The IANA timezone id, or `None` for pure-offset ocean zones.
    pub tzid: Option<String>,
    /// The feature's polygons (exterior ring first, holes after).
    pub polys: Vec<Poly>,
}

// i24 absolute global grid (~2.4 m lon / 1.2 m lat) — default; topo::encode_topology_q
// takes a `qbits` for i16/i24/i32.
//
// There is no native i24 type: quantized coords are STORED at i24 width in
// the container (see `push_i24`/`fixed_bytes`) but live in i32 in memory —
// these helpers quantize at the i24 default width, hence the names. The
// variable-width equivalents are local closures over a `qmax` (encode/topo).
pub use utz_common::{KM_PER_DEG, METERS_PER_DEG, dq_lat, dq_lon, q_lat, q_lon, qmax_for};

pub const QMAX_I24: f64 = qmax_for(24);

#[must_use]
pub fn q24_lon(lon: f64) -> i32 {
    q_lon(lon, QMAX_I24)
}
#[must_use]
pub fn q24_lat(lat: f64) -> i32 {
    q_lat(lat, QMAX_I24)
}

/// Appends `value`'s low three little-endian bytes, the stored form of an
/// i24-quantized coordinate.
pub fn push_i24(out: &mut Vec<u8>, value: i32) {
    let bytes = value.to_le_bytes();
    out.extend_from_slice(&bytes[0..3]);
}
#[must_use]
pub fn read_i24(bytes: &[u8]) -> i32 {
    let mut value = i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
    if value & 0x0080_0000 != 0 {
        value |= !0x00FF_FFFF;
    }
    value
}
