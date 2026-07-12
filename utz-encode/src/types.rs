//! Shared geometry types + quantization helpers for the encoder and measurements.

// Coordinate tuples are (lon, lat) — equivalently (x, y) with x = lon —
// everywhere in the workspace: builder f64 degrees and quantized i32 grid
// units alike. The aliases carry the semantic split: a Ring is closed
// (implicitly; no duplicated closing vertex), an Arc is an open polyline
// shared between the rings that reference it.
/// One closed ring: no duplicated closing vertex.
pub type Ring<T = f64> = Vec<(T, T)>;
/// Exterior ring first, then interior (hole) rings.
pub type Poly<T = f64> = Vec<Ring<T>>;
/// One open shared-boundary polyline (see `topo`); NOT `std::sync::Arc`.
pub type Arc<T = f64> = Vec<(T, T)>;

pub struct Feat {
    pub offset: f64,
    pub tzid: Option<String>,
    pub polys: Vec<Poly>,
}

// i24 absolute global grid (~2.4 m lon / 1.2 m lat) — default; topo::encode_topology_q
// takes a `qbits` for i16/i24/i32.
//
// There is no native i24 type: quantized coords are STORED at i24 width in
// the container (see `push_i24`/`fixed_bytes`) but live in i32 in memory —
// these helpers quantize at the i24 default width, hence the names. The
// variable-width equivalents are local closures over a `qmax` (encode/topo).
pub const QMAX_I24: f64 = 8_388_607.0; // 2^23 - 1

/// Half-range of an `i{bits}` quantization grid (`2^(bits-1) - 1`).
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "qmax = 2^(bits-1)-1 ≤ 2^31-1, exact in f64")]
pub fn qmax_for(bits: u32) -> f64 { ((1u64 << (bits - 1)) - 1) as f64 }

/// Quantize a longitude onto a grid with half-range `qmax` (see [`qmax_for`]).
#[must_use]
#[expect(clippy::cast_possible_truncation, reason = "lon bounded, |lon/180*qmax| < i32::MAX; float as saturates")]
pub fn q_lon(lon: f64, qmax: f64) -> i32 { (lon / 180.0 * qmax).round() as i32 }
/// Quantize a latitude onto a grid with half-range `qmax`.
#[must_use]
#[expect(clippy::cast_possible_truncation, reason = "lat bounded, |lat/90*qmax| < i32::MAX; float as saturates")]
pub fn q_lat(lat: f64, qmax: f64) -> i32 { (lat / 90.0 * qmax).round() as i32 }

#[must_use]
pub fn q24_lon(lon: f64) -> i32 { q_lon(lon, QMAX_I24) }
#[must_use]
pub fn q24_lat(lat: f64) -> i32 { q_lat(lat, QMAX_I24) }

pub fn push_i24(out: &mut Vec<u8>, v: i32) {
    let b = v.to_le_bytes();
    out.extend_from_slice(&b[0..3]);
}
#[must_use]
pub fn read_i24(b: &[u8]) -> i32 {
    let mut v = i32::from(b[0]) | (i32::from(b[1]) << 8) | (i32::from(b[2]) << 16);
    if v & 0x0080_0000 != 0 { v |= !0x00FF_FFFF; }
    v
}
