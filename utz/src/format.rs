//! Self-describing container parsing (PLAN.md §4). Layout is defined by the
//! encoder in `utz-build/src/encode.rs` — keep the two in sync.
//!
//! All multi-byte values little-endian. The parser stores OFFSETS into the
//! payload (no self-referential slices), so the same code serves borrowed
//! (`&'static`, zero-copy) and owned buffers.

use crate::Error;

// on-disk magic stays ASCII ("μ" is 2 bytes in UTF-8 and byte literals
// reject non-ASCII); the project brands as μTZ, the container as uTZ1
pub const MAGIC: [u8; 4] = *b"uTZ1";
pub const VERSION: u8 = 3; // v3: geom byte (arc-store encoding) in the header

/// Parsed header: every section position needed for O(1) access.
#[derive(Clone, Copy)]
pub struct Header {
    pub dataset: u8,
    pub quant_bits: u8,
    /// arc-store encoding: 0 = delta+varint, 1 = absolute fixed-width
    pub geom: u8,
    /// simplification algorithm the asset was built with (§14.8):
    /// 0 = RDP, 1 = Visvalingam, 2 = Imai–Iri — provenance, not decode logic
    pub simplify_algo: u8,
    /// cell size in degrees — fractional (e.g. 0.5) allowed
    pub grid_deg: f32,
    pub eps_m: f32,
    pub n_features: u16,
    // zone table
    pub str_offsets: usize, // u16[n_features+1]
    pub pool: usize,
    // arc store
    pub n_arcs: u32,
    pub arc_offsets: usize, // u32[n_arcs+1]
    pub arc_data: usize,
    // ring index
    pub feat_offsets: usize, // u32[n_features+1]
    pub ring_data: usize,
    // eager-cache reservation counts (v2): exact Vec sizes for `preload`
    // (coords is Σ referenced-arc vcounts — may only over-estimate)
    pub eager_coords: u32,
    pub eager_rings: u32,
    pub eager_polys: u32,
    // grid
    pub ncols: u16,
    pub nrows: u16,
    pub primary: usize, // u16[ncols*nrows]
    pub uniq: u16,
    pub list_offsets: usize, // u16[uniq+1]
    pub list_ids: usize,
}

pub fn read_u16(b: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes([b[pos], b[pos + 1]])
}
pub fn read_u32(b: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]])
}

/// Fixed-width signed coord: 2/3/4 bytes little-endian, sign-extended.
pub fn read_fixed(b: &[u8], pos: usize, qbits: u8) -> i32 {
    match qbits {
        16 => read_u16(b, pos) as i16 as i32,
        24 => {
            let v = (b[pos] as i32) | ((b[pos + 1] as i32) << 8) | ((b[pos + 2] as i32) << 16);
            if v & 0x0080_0000 != 0 { v | !0x00FF_FFFF } else { v }
        }
        _ => read_u32(b, pos) as i32,
    }
}
pub const fn fixed_bytes(qbits: u8) -> usize {
    (qbits as usize + 7) / 8
}

/// Varint; returns (value, next_pos).
pub fn read_varint(b: &[u8], mut pos: usize) -> (u64, usize) {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let byte = b[pos];
        pos += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return (v, pos);
        }
        shift += 7;
    }
}
pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Validate the outer header; returns (codec, raw_len, payload_start).
/// `raw_len` is the UNCOMPRESSED payload size (single exact allocation).
pub fn outer(bytes: &[u8]) -> Result<(u8, usize, usize), Error> {
    if bytes.len() < 10 || bytes[0..4] != MAGIC || bytes[4] != VERSION {
        return Err(Error::BadFormat);
    }
    Ok((bytes[5], read_u32(bytes, 6) as usize, 10))
}

/// Parse the payload header + section directory.
pub fn parse(p: &[u8]) -> Result<Header, Error> {
    let need = |n: usize| if p.len() < n { Err(Error::BadFormat) } else { Ok(()) };
    need(13)?;
    let dataset = p[0];
    let quant_bits = p[1];
    let simplify_algo = p[2];
    let geom = p[3];
    let grid_deg = f32::from_le_bytes([p[4], p[5], p[6], p[7]]);
    if !matches!(quant_bits, 16 | 24 | 32) || geom > 1 || !(grid_deg > 0.0) {
        return Err(Error::BadFormat);
    }
    let eps_m = f32::from_le_bytes([p[8], p[9], p[10], p[11]]);
    let rel_len = p[12] as usize;
    let mut pos = 13 + rel_len; // tzbb_release skipped (read via header_release)
    need(pos + 26)?;
    let n_features = read_u16(p, pos);
    pos += 2;
    let arcs_off = read_u32(p, pos) as usize;
    let rings_off = read_u32(p, pos + 4) as usize;
    let grid_off = read_u32(p, pos + 8) as usize;
    let eager_coords = read_u32(p, pos + 12);
    let eager_rings = read_u32(p, pos + 16);
    let eager_polys = read_u32(p, pos + 20);
    pos += 24;

    let str_offsets = pos;
    let pool = str_offsets + (n_features as usize + 1) * 2;

    need(arcs_off + 4)?;
    let n_arcs = read_u32(p, arcs_off);
    let arc_offsets = arcs_off + 4;
    let arc_data = arc_offsets + (n_arcs as usize + 1) * 4;

    let feat_offsets = rings_off;
    let ring_data = feat_offsets + (n_features as usize + 1) * 4;

    need(grid_off + 4)?;
    let ncols = read_u16(p, grid_off);
    let nrows = read_u16(p, grid_off + 2);
    let primary = grid_off + 4;
    let after_primary = primary + ncols as usize * nrows as usize * 2;
    need(after_primary + 2)?;
    let uniq = read_u16(p, after_primary);
    let list_offsets = after_primary + 2;
    let list_ids = list_offsets + (uniq as usize + 1) * 2;
    need(list_ids)?;

    Ok(Header {
        dataset, quant_bits, geom, simplify_algo, grid_deg, eps_m, n_features,
        str_offsets, pool,
        n_arcs, arc_offsets, arc_data,
        feat_offsets, ring_data,
        eager_coords, eager_rings, eager_polys,
        ncols, nrows, primary, uniq, list_offsets, list_ids,
    })
}

/// TZBB release string recorded in the header.
pub fn release(p: &[u8]) -> &[u8] {
    let n = p[12] as usize;
    &p[13..13 + n]
}
