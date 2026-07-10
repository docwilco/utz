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
pub const VERSION: u8 = 7; // v7: flags byte + image coords at quant width
                           // (i24 packed, optional ring alignment); v6 12-byte
                           // outer + EagerImage; v5 bbox; v4 poly grid; v3 geom
/// Outer container header length (v6): magic4 + version + codec + `raw_len` u32
/// + 2 reserved bytes so a 4-aligned container gives a 4-aligned payload.
pub const OUTER_LEN: usize = 12;

/// Parsed header: every section position needed for O(1) access.
#[derive(Clone, Copy)]
pub struct Header {
    pub dataset: u8,
    pub quant_bits: u8,
    /// geometry encoding: 0 = delta+varint arcs, 1 = fixed-width arcs,
    /// 2 = `EagerImage` (flattened per-ring coords at quant width, no arc store)
    pub geom: u8,
    /// reserved, must be zero (room for future format flags)
    pub flags: u8,
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
    // ring index (v4: per-poly records; grid candidates are polys)
    /// poly id → feature id, u16[`eager_polys`]
    pub parent: usize,
    pub poly_offsets: usize, // u32[eager_polys+1]
    pub ring_data: usize,
    // eager-image sections (geom=2 only; usize::MAX otherwise): the
    // preload-cache layout serialized — coords 4-aligned within the payload
    pub img_coords: usize, // (i32, i32)[eager_coords]
    pub img_ring_ends: usize, // u32[eager_rings]
    pub img_polys: usize, // (bbox [i32; 4] + ring_end u32)[eager_polys]
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

#[must_use]
pub fn read_u16(b: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes([b[pos], b[pos + 1]])
}
#[must_use]
pub fn read_u32(b: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]])
}

/// Fixed-width signed coord: 2/3/4 bytes little-endian, sign-extended.
#[must_use]
pub fn read_fixed(b: &[u8], pos: usize, qbits: u8) -> i32 {
    match qbits {
        16 => i32::from(read_u16(b, pos) as i16),
        24 => {
            let v = i32::from(b[pos]) | (i32::from(b[pos + 1]) << 8) | (i32::from(b[pos + 2]) << 16);
            if v & 0x0080_0000 != 0 { v | !0x00FF_FFFF } else { v }
        }
        _ => read_u32(b, pos) as i32,
    }
}
#[must_use]
pub const fn fixed_bytes(qbits: u8) -> usize {
    (qbits as usize).div_ceil(8)
}

/// Varint; returns (value, `next_pos`).
#[must_use]
pub fn read_varint(b: &[u8], mut pos: usize) -> (u64, usize) {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let byte = b[pos];
        pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (v, pos);
        }
        shift += 7;
    }
}
#[must_use]
pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Validate the outer header; returns (codec, `raw_len`, `payload_start`).
/// `raw_len` is the UNCOMPRESSED payload size (single exact allocation).
///
/// # Errors
/// [`Error::BadFormat`] if the bytes are too short or the magic/version
/// don't match.
pub fn outer(bytes: &[u8]) -> Result<(u8, usize, usize), Error> {
    if bytes.len() < OUTER_LEN || bytes[0..4] != MAGIC || bytes[4] != VERSION {
        return Err(Error::BadFormat);
    }
    Ok((bytes[5], read_u32(bytes, 6) as usize, OUTER_LEN))
}

/// Parse the payload header + section directory.
///
/// # Errors
/// [`Error::BadFormat`] for invalid header fields or a section overrunning
/// the payload; [`Error::Geometry`] if the geometry encoding has no
/// compiled-in decoder.
pub fn parse(p: &[u8]) -> Result<Header, Error> {
    let need = |n: usize| if p.len() < n { Err(Error::BadFormat) } else { Ok(()) };
    need(14)?;
    let dataset = p[0];
    let quant_bits = p[1];
    let simplify_algo = p[2];
    let geom = p[3];
    let flags = p[4];
    let grid_deg = f32::from_le_bytes([p[5], p[6], p[7], p[8]]);
    if !matches!(quant_bits, 16 | 24 | 32)
        || geom > 3
        || flags != 0
        || grid_deg.is_nan()
        || grid_deg <= 0.0
    {
        return Err(Error::BadFormat);
    }
    // a valid geom byte whose decoder isn't compiled in is refused loudly
    let compiled = match geom {
        0 => cfg!(feature = "geom-varint"),
        1 => cfg!(feature = "geom-fixed"),
        2 => cfg!(feature = "geom-image"),
        _ => cfg!(feature = "geom-coarse"),
    };
    if !compiled {
        return Err(Error::Geometry);
    }
    let eps_m = f32::from_le_bytes([p[9], p[10], p[11], p[12]]);
    let rel_len = p[13] as usize;
    let mut pos = 14 + rel_len; // tzbb_release skipped (read via header_release)
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

    let n_polys = eager_polys as usize;
    let parent = rings_off;
    let (n_arcs, arc_offsets, arc_data, poly_offsets, ring_data);
    let (img_coords, img_ring_ends, img_polys);
    if geom == 3 {
        // coarse: no geometry sections at all — just parent + grid
        need(parent + n_polys * 2)?;
        (n_arcs, arc_offsets, arc_data) = (0, usize::MAX, usize::MAX);
        (poly_offsets, ring_data) = (usize::MAX, usize::MAX);
        (img_coords, img_ring_ends, img_polys) = (usize::MAX, usize::MAX, usize::MAX);
    } else if geom == 2 {
        // EagerImage: the preload-cache layout in place of arc store + ring
        // records. Coords must be 4-aligned within the payload (encoder
        // pads; the v6 12-byte outer header preserves it in flash).
        img_coords = arcs_off;
        if img_coords % 4 != 0 {
            return Err(Error::BadFormat);
        }
        // coords at quant width (v7): 4 / 6 / 8 bytes per vertex
        let vb = 2 * fixed_bytes(quant_bits);
        img_ring_ends = img_coords + eager_coords as usize * vb;
        img_polys = img_ring_ends + eager_rings as usize * 4;
        need(img_polys + n_polys * 20)?;
        // the flattened image is self-delimiting — the counts must agree
        if eager_rings > 0
            && read_u32(p, img_ring_ends + (eager_rings as usize - 1) * 4) != eager_coords
        {
            return Err(Error::BadFormat);
        }
        (n_arcs, arc_offsets, arc_data) = (0, usize::MAX, usize::MAX);
        (poly_offsets, ring_data) = (usize::MAX, usize::MAX);
    } else {
        need(arcs_off + 4)?;
        n_arcs = read_u32(p, arcs_off);
        arc_offsets = arcs_off + 4;
        arc_data = arc_offsets + (n_arcs as usize + 1) * 4;
        poly_offsets = parent + n_polys * 2;
        ring_data = poly_offsets + (n_polys + 1) * 4;
        (img_coords, img_ring_ends, img_polys) = (usize::MAX, usize::MAX, usize::MAX);
    }

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
        dataset, quant_bits, geom, flags, simplify_algo, grid_deg, eps_m, n_features,
        str_offsets, pool,
        n_arcs, arc_offsets, arc_data,
        parent, poly_offsets, ring_data,
        img_coords, img_ring_ends, img_polys,
        eager_coords, eager_rings, eager_polys,
        ncols, nrows, primary, uniq, list_offsets, list_ids,
    })
}

/// TZBB release string recorded in the header.
#[must_use]
pub fn release(p: &[u8]) -> &[u8] {
    let n = p[13] as usize;
    &p[14..14 + n]
}
