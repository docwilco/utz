//! Shared geometry types + quantization helpers for the encoder and measurements.

/// exterior ring first, then interior (hole) rings; no duplicated closing vertex.
pub type Ring = Vec<(f64, f64)>;
pub type Poly = Vec<Ring>;

pub struct Feat {
    pub offset: f64,
    pub tzid: Option<String>,
    pub polys: Vec<Poly>,
}

// i24 absolute global grid (~2.4 m lon / 1.2 m lat) — default; topo::encode_topology_q
// takes a `qbits` for i16/i24/i32.
pub const QMAX: f64 = 8_388_607.0; // 2^23 - 1
pub fn qx(lon: f64) -> i32 { (lon / 180.0 * QMAX).round() as i32 }
pub fn qy(lat: f64) -> i32 { (lat / 90.0 * QMAX).round() as i32 }

pub fn push_i24(out: &mut Vec<u8>, v: i32) {
    let b = v.to_le_bytes();
    out.extend_from_slice(&b[0..3]);
}
pub fn read_i24(b: &[u8]) -> i32 {
    let mut v = (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16);
    if v & 0x0080_0000 != 0 { v |= !0x00FF_FFFF; }
    v
}
