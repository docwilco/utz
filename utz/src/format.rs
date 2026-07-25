//! Self-describing container parsing. The payload header is the shared
//! [`PayloadHeader`] record (utz-common) — the encoder in utz-encode
//! serializes the same struct, so the two cannot drift.
//!
//! All multi-byte values little-endian. The parser stores OFFSETS into the
//! payload (no self-referential slices), so the same code serves borrowed
//! (`&'static`, zero-copy) and owned buffers.

use scroll::Pread;
use utz_common::{
    Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo, PAYLOAD_HEADER_LEN,
};
pub use utz_common::{MAGIC, VERSION};

use crate::{Error, Result};

/// Outer container header length: magic4 + version + codec + `raw_len` u32
/// + 2 reserved bytes so a 4-aligned container gives a 4-aligned payload.
pub const OUTER_LEN: usize = 12;

/// Parsed header: every section position needed for O(1) access.
#[derive(Clone, Copy)]
pub struct PayloadLayout {
    pub dataset: Dataset,
    pub quant_bits: QuantBits,
    pub geom: GeomEncoding,
    /// reserved, must be zero (room for future format flags)
    pub flags: u16,
    /// provenance, not decode logic
    pub simplify_algo: SimplifyAlgo,
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
    // ring index (per-poly records; grid candidates are polys)
    /// poly id → feature id, `u16[eager_polys]`
    pub parent: usize,
    pub poly_offsets: usize, // u32[eager_polys+1]
    pub ring_data: usize,
    // eager-image sections (geom=2 only; usize::MAX otherwise): the
    // preload-cache layout serialized — coords 4-aligned within the payload
    pub img_coords: usize,    // (i32, i32)[eager_coords]
    pub img_ring_ends: usize, // u32[eager_rings]
    pub img_polys: usize,     // (bbox [i32; 4] + ring_end u32)[eager_polys]
    // eager-cache reservation counts: exact Vec sizes for `preload`
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
    // TZBB release string (the payload tail)
    pub release_off: usize,
    pub release_len: usize,
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
pub fn read_fixed(b: &[u8], pos: usize, quant_bits: QuantBits) -> i32 {
    match quant_bits {
        QuantBits::Bits16 => i32::from(read_u16(b, pos).cast_signed()),
        QuantBits::Bits24 => {
            let v =
                i32::from(b[pos]) | (i32::from(b[pos + 1]) << 8) | (i32::from(b[pos + 2]) << 16);
            if v & 0x0080_0000 != 0 {
                v | !0x00FF_FFFF
            } else {
                v
            }
        }
        QuantBits::Bits32 => read_u32(b, pos).cast_signed(),
    }
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
    (v >> 1).cast_signed() ^ -((v & 1).cast_signed())
}

/// Validate the outer header; returns (codec, `raw_len`, `payload_start`).
/// `raw_len` is the UNCOMPRESSED payload size (single exact allocation).
///
/// # Errors
/// [`Error::Truncated`] / [`Error::BadMagic`] / [`Error::UnsupportedVersion`]
/// if the bytes are too short or the magic/version don't match.
pub fn outer(bytes: &[u8]) -> Result<(u8, usize, usize)> {
    if bytes.len() < OUTER_LEN {
        return Err(Error::Truncated);
    }
    if bytes[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    if bytes[4] != VERSION {
        return Err(Error::UnsupportedVersion(bytes[4]));
    }
    Ok((bytes[5], read_u32(bytes, 6) as usize, OUTER_LEN))
}

/// Parse the payload header + section directory.
///
/// # Errors
/// [`Error::InvalidHeaderField`] for invalid header fields;
/// [`Error::SectionOverrun`] for a section overrunning the payload;
/// [`Error::GeometryNotCompiledIn`] if the geometry encoding has no
/// compiled-in decoder.
pub fn parse(p: &[u8]) -> Result<PayloadLayout> {
    // an invalid enum byte (quant_bits/geom/simplify_algo/dataset) fails the
    // header read itself as BadInput; running out of bytes is an overrun
    let h: PayloadHeader = p.pread_with(0, scroll::LE).map_err(|source| match source {
        scroll::Error::BadInput { .. } => Error::InvalidHeaderField,
        _ => Error::SectionOverrun,
    })?;
    if h.flags != 0 || h.grid_deg.is_nan() || h.grid_deg <= 0.0 {
        return Err(Error::InvalidHeaderField);
    }
    // a valid geom byte whose decoder isn't compiled in is refused loudly
    let compiled = match h.geom {
        GeomEncoding::DeltaVarint => cfg!(feature = "geom-varint"),
        GeomEncoding::Fixed => cfg!(feature = "geom-fixed"),
        GeomEncoding::EagerImage => cfg!(feature = "geom-image"),
        GeomEncoding::Coarse => cfg!(feature = "geom-coarse"),
    };
    if !compiled {
        return Err(Error::GeometryNotCompiledIn(h.geom.byte()));
    }

    let str_offsets = PAYLOAD_HEADER_LEN;
    let pool = str_offsets + (h.n_features as usize + 1) * 2;

    let n_polys = h.eager_polys as usize;
    let (arcs_off, parent) = (h.arcs_off as usize, h.rings_off as usize);
    let sections = match h.geom {
        GeomEncoding::Coarse => coarse_sections(p, parent, n_polys)?,
        GeomEncoding::EagerImage => image_sections(
            p,
            h.quant_bits,
            arcs_off,
            n_polys,
            h.eager_coords,
            h.eager_rings,
        )?,
        GeomEncoding::DeltaVarint | GeomEncoding::Fixed => {
            arc_sections(p, arcs_off, parent, n_polys, h.n_arcs)?
        }
    };

    let primary = h.grid_off as usize;
    let list_offsets = primary + h.ncols as usize * h.nrows as usize * 2;
    let list_ids = list_offsets + (h.uniq as usize + 1) * 2;
    need(p, list_ids)?;
    let (release_off, release_len) = (h.release_off as usize, h.release_len as usize);
    need(p, release_off + release_len)?;

    Ok(PayloadLayout {
        dataset: h.dataset,
        quant_bits: h.quant_bits,
        geom: h.geom,
        flags: h.flags,
        simplify_algo: h.simplify_algo,
        grid_deg: h.grid_deg,
        eps_m: h.eps_m,
        n_features: h.n_features,
        str_offsets,
        pool,
        n_arcs: sections.n_arcs,
        arc_offsets: sections.arc_offsets,
        arc_data: sections.arc_data,
        parent,
        poly_offsets: sections.poly_offsets,
        ring_data: sections.ring_data,
        img_coords: sections.img_coords,
        img_ring_ends: sections.img_ring_ends,
        img_polys: sections.img_polys,
        eager_coords: h.eager_coords,
        eager_rings: h.eager_rings,
        eager_polys: h.eager_polys,
        ncols: h.ncols,
        nrows: h.nrows,
        primary,
        uniq: h.uniq,
        list_offsets,
        list_ids,
        release_off,
        release_len,
    })
}

/// Reject a section end past the payload end.
fn need(p: &[u8], end: usize) -> Result<()> {
    if p.len() < end {
        Err(Error::SectionOverrun)
    } else {
        Ok(())
    }
}

/// Geometry-dependent section offsets for [`PayloadLayout`]; the fields a given
/// encoding doesn't use stay at [`GeometrySections::NONE`]'s markers.
struct GeometrySections {
    n_arcs: u32,
    arc_offsets: usize,
    arc_data: usize,
    poly_offsets: usize,
    ring_data: usize,
    img_coords: usize,
    img_ring_ends: usize,
    img_polys: usize,
}

impl GeometrySections {
    /// No sections present: zero arcs, every offset `usize::MAX`.
    const NONE: GeometrySections = GeometrySections {
        n_arcs: 0,
        arc_offsets: usize::MAX,
        arc_data: usize::MAX,
        poly_offsets: usize::MAX,
        ring_data: usize::MAX,
        img_coords: usize::MAX,
        img_ring_ends: usize::MAX,
        img_polys: usize::MAX,
    };
}

/// Coarse containers (geom 3) have no geometry sections at all — just the
/// parent table + grid.
fn coarse_sections(p: &[u8], parent: usize, n_polys: usize) -> Result<GeometrySections> {
    need(p, parent + n_polys * 2)?;
    Ok(GeometrySections::NONE)
}

/// `EagerImage` (geom 2): the preload-cache layout in place of arc store +
/// ring records. Coords must be 4-aligned within the payload (encoder pads;
/// the 12-byte outer header preserves it in flash).
fn image_sections(
    p: &[u8],
    quant_bits: QuantBits,
    img_coords: usize,
    n_polys: usize,
    eager_coords: u32,
    eager_rings: u32,
) -> Result<GeometrySections> {
    if !img_coords.is_multiple_of(4) {
        return Err(Error::ImageSectionMisaligned);
    }
    // coords at quant width: 4 / 6 / 8 bytes per vertex
    let vertex_bytes = 2 * quant_bits.bytes();
    let img_ring_ends = img_coords + eager_coords as usize * vertex_bytes;
    let img_polys = img_ring_ends + eager_rings as usize * 4;
    need(p, img_polys + n_polys * 20)?;
    // the flattened image is self-delimiting — the counts must agree
    if eager_rings > 0
        && read_u32(p, img_ring_ends + (eager_rings as usize - 1) * 4) != eager_coords
    {
        return Err(Error::ImageCountsDisagree);
    }
    Ok(GeometrySections {
        img_coords,
        img_ring_ends,
        img_polys,
        ..GeometrySections::NONE
    })
}

/// Arc-store encodings (geom 0/1): arc offsets + data, per-poly ring records.
fn arc_sections(
    p: &[u8],
    arcs_off: usize,
    parent: usize,
    n_polys: usize,
    n_arcs: u32,
) -> Result<GeometrySections> {
    let arc_offsets = arcs_off;
    let arc_data = arc_offsets + (n_arcs as usize + 1) * 4;
    need(p, arc_data)?;
    let poly_offsets = parent + n_polys * 2;
    let ring_data = poly_offsets + (n_polys + 1) * 4;
    Ok(GeometrySections {
        n_arcs,
        arc_offsets,
        arc_data,
        poly_offsets,
        ring_data,
        ..GeometrySections::NONE
    })
}

/// TZBB release string recorded in the header.
#[must_use]
pub fn release<'p>(h: &PayloadLayout, p: &'p [u8]) -> &'p [u8] {
    &p[h.release_off..h.release_off + h.release_len]
}
