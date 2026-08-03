//! The asset container format and its parsing. Using μTZ does not
//! require this module: every [`Finder`](crate::Finder) constructor
//! calls it internally. It is public for the cases where you want to
//! look at an asset without loading it: sniff its provenance (dataset,
//! ε, codec, release) or cheaply validate an OTA download via
//! [`outer()`] + [`parse()`] before committing it to flash.
//!
//! Container layout: an 8-byte plaintext prologue ([`MAGIC`],
//! [`VERSION`], reserved padding), the 64-byte plaintext payload header
//! (the shared [`PayloadHeader`] record, which both the encoder and this
//! parser serialize through, so the two cannot drift), then the section
//! blob: zone strings, geometry sections per the encoding, grid tables,
//! and the release string, compressed as one unit when a codec is set.
//! Because the header stays plaintext, an asset is identified and
//! validated BEFORE any decompression.
//!
//! All multi-byte values little-endian. The parser stores OFFSETS into the
//! section blob (no self-referential slices), so the same code serves
//! borrowed (`&'static`, zero-copy) and owned buffers.

use scroll::Pread;
use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};
pub use utz_common::{MAGIC, PAYLOAD_HEADER_LEN, PROLOGUE_LEN, VERSION};

use crate::{Error, Result};

/// Parsed header: every section position needed for O(1) access.
#[derive(Clone, Copy)]
pub struct PayloadLayout {
    /// the section blob's compression codec
    pub codec: Codec,
    /// the blob's decompressed size (every stored offset stays within it)
    pub sections_len: usize,
    pub dataset: Dataset,
    pub quant_bits: QuantBits,
    pub geom: GeomEncoding,
    /// reserved, must be zero (room for future format flags)
    pub flags: u16,
    /// provenance, not decode logic
    pub simplify_algo: SimplifyAlgo,
    /// cell size in degrees; fractional (e.g. 0.5) allowed
    pub grid_deg: f32,
    pub eps_m: f32,
    /// population-density weight floor ×1e-4 the asset was built with
    /// (0 = unweighted; provenance, exposed as
    /// [`Finder::density_weight_floor()`](crate::Finder::density_weight_floor))
    pub w_min_e4: u16,
    pub n_features: u16,
    /// The tzid pool; the zone string-offset table (`u16[n_features+1]`)
    /// starts the section blob at offset 0, this pool follows it.
    pub pool: usize,
    /// Arcs in the arc store.
    pub n_arcs: u32,
    /// Arc offset table, `u32[n_arcs+1]`.
    pub arc_offsets: usize,
    /// The arc coordinate data the offset table indexes.
    pub arc_data: usize,
    /// Poly id → feature id, `u16[eager_polys]` (grid candidates are
    /// polys; per-poly ring records follow).
    pub parent: usize,
    /// Per-poly ring-record offsets, `u32[eager_polys+1]`.
    pub poly_offsets: usize,
    /// The ring records the poly offsets index.
    pub ring_data: usize,
    /// `FullRings` coords, `(i32, i32)[eager_coords]`, 4-aligned within
    /// the payload (geom=2 only; `usize::MAX` otherwise — the
    /// preload-cache layout serialized).
    pub full_coords: usize,
    /// `FullRings` ring ends, `u32[eager_rings]` (geom=2 only).
    pub full_ring_ends: usize,
    /// `FullRings` per-poly records, bbox `[i32; 4]` + ring-end `u32`
    /// (geom=2 only).
    pub full_polys: usize,
    /// Eager-cache coordinate reservation: exact `Vec` size for
    /// `preload` (Σ referenced-arc vcounts; may only over-estimate).
    pub eager_coords: u32,
    /// Eager-cache ring reservation.
    pub eager_rings: u32,
    /// Eager-cache poly reservation.
    pub eager_polys: u32,
    /// Grid columns.
    pub ncols: u16,
    /// Grid rows.
    pub nrows: u16,
    /// Primary grid table, `u16[ncols*nrows]`.
    pub primary: usize,
    /// Distinct border-cell candidate lists.
    pub uniq: u16,
    /// Candidate-list offsets, `u16[uniq+1]`.
    pub list_offsets: usize,
    /// The interned candidate lists the offsets index.
    pub list_ids: usize,
    /// TZBB release string offset (the payload tail).
    pub release_off: usize,
    /// TZBB release string length.
    pub release_len: usize,
}

/// Little-endian u16 at `pos`.
///
/// # Panics
/// If `pos + 2` exceeds `b` (all `read_*` helpers assume offsets already
/// validated by [`parse()`]).
#[must_use]
pub fn read_u16(b: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes([b[pos], b[pos + 1]])
}
/// Little-endian u32 at `pos`.
///
/// # Panics
/// If `pos + 4` exceeds `b`.
#[must_use]
pub fn read_u32(b: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([b[pos], b[pos + 1], b[pos + 2], b[pos + 3]])
}

/// Fixed-width signed coord: 2/3/4 bytes little-endian, sign-extended.
///
/// # Panics
/// If the coord's bytes run past `b`.
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
///
/// # Panics
/// If the varint runs past `b`.
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
/// Undo zigzag encoding: map `0, 1, 2, 3, …` back to `0, -1, 1, -2, …`.
#[must_use]
pub fn unzigzag(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ -((v & 1).cast_signed())
}

/// Validate the prologue (format identity only: magic + version) and
/// return the payload header's offset.
///
/// # Errors
/// [`Error::Truncated`] / [`Error::BadMagic`] / [`Error::UnsupportedVersion`]
/// if the bytes are too short or the magic/version don't match.
pub fn outer(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < PROLOGUE_LEN {
        return Err(Error::Truncated);
    }
    if bytes[0..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    if bytes[4] != VERSION {
        return Err(Error::UnsupportedVersion(bytes[4]));
    }
    Ok(PROLOGUE_LEN)
}

/// Parse the plaintext payload header. Every section bound is validated
/// against the header's own declared blob size, so a container is fully
/// checked before any decompression happens.
///
/// # Errors
/// [`Error::Truncated`] if `header` is short; [`Error::InvalidHeaderField`]
/// for invalid header fields (including unknown codec / geometry /
/// quantization / dataset bytes); [`Error::SectionOverrun`] for a section
/// overrunning the blob; [`Error::GeometryNotCompiledIn`] if the geometry
/// encoding has no compiled-in decoder.
pub fn parse(header: &[u8]) -> Result<PayloadLayout> {
    // an invalid enum byte (quant_bits/geom/simplify_algo/dataset) fails the
    // header read itself as BadInput; running out of bytes means the source
    // ends inside the header
    let h: PayloadHeader = header
        .pread_with(0, scroll::LE)
        .map_err(|source| match source {
            scroll::Error::BadInput { .. } => Error::InvalidHeaderField,
            _ => Error::Truncated,
        })?;
    if h.flags != 0
        || h.reserved != 0
        || h.w_min_e4 > 10_000
        || h.grid_deg.is_nan()
        || h.grid_deg <= 0.0
        || h.ncols == 0
        || h.nrows == 0
    {
        return Err(Error::InvalidHeaderField);
    }
    let sections_len = h.raw_len as usize;
    // a valid geom byte whose decoder isn't compiled in is refused loudly
    let compiled = match h.geom {
        GeomEncoding::VarintArcs => cfg!(feature = "geom-varint-arcs"),
        GeomEncoding::FixedWidthArcs => cfg!(feature = "geom-fixed-width-arcs"),
        GeomEncoding::FullRings => cfg!(feature = "geom-full-rings"),
        GeomEncoding::Coarse => cfg!(feature = "geom-coarse"),
    };
    if !compiled {
        return Err(Error::GeometryNotCompiledIn(h.geom));
    }

    let pool = (h.n_features as usize + 1) * 2;

    let n_polys = h.eager_polys as usize;
    let (arcs_off, parent) = (h.arcs_off as usize, h.rings_off as usize);
    let sections = match h.geom {
        GeomEncoding::Coarse => coarse_sections(sections_len, parent, n_polys)?,
        GeomEncoding::FullRings => full_rings_sections(
            sections_len,
            h.quant_bits,
            arcs_off,
            n_polys,
            h.eager_coords,
            h.eager_rings,
        )?,
        GeomEncoding::VarintArcs | GeomEncoding::FixedWidthArcs => {
            arc_sections(sections_len, arcs_off, parent, n_polys, h.n_arcs)?
        }
    };

    let primary = h.grid_off as usize;
    let list_offsets = primary + h.ncols as usize * h.nrows as usize * 2;
    let list_ids = list_offsets + (h.uniq as usize + 1) * 2;
    need(sections_len, list_ids)?;
    let (release_off, release_len) = (h.release_off as usize, h.release_len as usize);
    need(sections_len, release_off + release_len)?;

    Ok(PayloadLayout {
        codec: h.codec,
        sections_len,
        dataset: h.dataset,
        quant_bits: h.quant_bits,
        geom: h.geom,
        flags: h.flags,
        simplify_algo: h.simplify_algo,
        grid_deg: h.grid_deg,
        eps_m: h.eps_m,
        w_min_e4: h.w_min_e4,
        n_features: h.n_features,
        pool,
        n_arcs: sections.n_arcs,
        arc_offsets: sections.arc_offsets,
        arc_data: sections.arc_data,
        parent,
        poly_offsets: sections.poly_offsets,
        ring_data: sections.ring_data,
        full_coords: sections.full_coords,
        full_ring_ends: sections.full_ring_ends,
        full_polys: sections.full_polys,
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

/// Reject a section end past the blob end.
/// Validate every cross-reference inside the (decompressed) section
/// tables that [`crate::Finder::lookup()`] follows without further
/// checks: zone-string offsets, the parent table, grid cell values,
/// and the candidate lists. One linear pass at load time, so a corrupt
/// or hostile asset is refused with a typed error instead of panicking
/// a lookup later.
///
/// # Errors
/// [`Error::TableOutOfRange`] on the first out-of-range reference.
pub fn check_tables(p: &[u8], h: &PayloadLayout) -> Result<()> {
    let bad = Error::TableOutOfRange;
    let nf = usize::from(h.n_features);
    // zone-string offset table: u16[n_features+1] at 0, monotone, within
    // the payload once rebased onto the pool
    let mut prev = 0u16;
    for i in 0..=nf {
        let off = crate::format::read_u16(p, i * 2);
        if off < prev || h.pool + usize::from(off) > h.sections_len {
            return Err(bad);
        }
        prev = off;
    }
    // parent table: poly id → feature id
    let polys = h.eager_polys as usize;
    for i in 0..polys {
        if usize::from(read_u16(p, h.parent + i * 2)) >= nf {
            return Err(bad);
        }
    }
    // candidate lists: u16[uniq+1] offsets monotone and in range, ids are
    // poly ids
    let uniq = usize::from(h.uniq);
    let list_len = (h.release_off.saturating_sub(h.list_ids)) / 2;
    let mut prev = 0u16;
    for i in 0..=uniq {
        let off = read_u16(p, h.list_offsets + i * 2);
        if off < prev || usize::from(off) > list_len {
            return Err(bad);
        }
        prev = off;
    }
    let end = usize::from(prev);
    for i in 0..end {
        if usize::from(read_u16(p, h.list_ids + i * 2)) >= polys {
            return Err(bad);
        }
    }
    // grid: interior cells name a feature, border cells a candidate list
    for i in 0..(h.ncols as usize * h.nrows as usize) {
        let v = read_u16(p, h.primary + i * 2);
        if v == utz_common::NO_ZONE {
            continue;
        }
        if v & 0x8000 == 0 {
            if usize::from(v) >= nf {
                return Err(bad);
            }
        } else if usize::from(v & 0x7FFF) >= uniq {
            return Err(bad);
        }
    }
    Ok(())
}

fn need(sections_len: usize, end: usize) -> Result<()> {
    if sections_len < end {
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
    full_coords: usize,
    full_ring_ends: usize,
    full_polys: usize,
}

impl GeometrySections {
    /// No sections present: zero arcs, every offset `usize::MAX`.
    const NONE: GeometrySections = GeometrySections {
        n_arcs: 0,
        arc_offsets: usize::MAX,
        arc_data: usize::MAX,
        poly_offsets: usize::MAX,
        ring_data: usize::MAX,
        full_coords: usize::MAX,
        full_ring_ends: usize::MAX,
        full_polys: usize::MAX,
    };
}

/// Coarse containers (geom 3) have no geometry sections at all: just the
/// parent table + grid.
fn coarse_sections(sections_len: usize, parent: usize, n_polys: usize) -> Result<GeometrySections> {
    need(sections_len, parent + n_polys * 2)?;
    Ok(GeometrySections::NONE)
}

/// `FullRings` (geom 2): the preload-cache layout in place of arc store +
/// ring records. Coords must be 4-aligned within the payload (encoder pads;
/// the 12-byte outer header preserves it in flash).
fn full_rings_sections(
    sections_len: usize,
    quant_bits: QuantBits,
    full_coords: usize,
    n_polys: usize,
    eager_coords: u32,
    eager_rings: u32,
) -> Result<GeometrySections> {
    if !full_coords.is_multiple_of(4) {
        return Err(Error::FullRingsSectionMisaligned);
    }
    // coords at quant width: 4 / 6 / 8 bytes per vertex
    let vertex_bytes = 2 * quant_bits.bytes();
    let full_ring_ends = full_coords + eager_coords as usize * vertex_bytes;
    let full_polys = full_ring_ends + eager_rings as usize * 4;
    need(sections_len, full_polys + n_polys * 20)?;
    // (the ring-end/coordinate-count agreement check needs section bytes and
    // runs post-decompression in the finder's check_full_rings)
    Ok(GeometrySections {
        full_coords,
        full_ring_ends,
        full_polys,
        ..GeometrySections::NONE
    })
}

/// Arc-store encodings (geom 0/1): arc offsets + data, per-poly ring records.
fn arc_sections(
    sections_len: usize,
    arcs_off: usize,
    parent: usize,
    n_polys: usize,
    n_arcs: u32,
) -> Result<GeometrySections> {
    let arc_offsets = arcs_off;
    let arc_data = arc_offsets + (n_arcs as usize + 1) * 4;
    need(sections_len, arc_data)?;
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

#[cfg(all(test, feature = "geom-varint-arcs"))]
mod tests {
    use super::*;
    use scroll::Pwrite;
    use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

    /// A minimal consistent varint-arcs header: one feature with an empty
    /// tzid, no arcs, no polys, a 1×1 grid whose cell is `NO_ZONE`, and a
    /// 20-byte section blob.
    fn tiny_header() -> PayloadHeader {
        PayloadHeader {
            arcs_off: 8,
            rings_off: 12,
            grid_off: 16,
            release_off: 20,
            eager_coords: 0,
            eager_rings: 0,
            eager_polys: 0,
            n_arcs: 0,
            raw_len: 20,
            grid_deg: 360.0,
            eps_m: 500.0,
            n_features: 1,
            ncols: 1,
            nrows: 1,
            uniq: 0,
            release_len: 0,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits24,
            simplify_algo: SimplifyAlgo::Rdp,
            geom: GeomEncoding::VarintArcs,
            codec: Codec::Uncompressed,
            w_min_e4: 10,
            reserved: 0,
        }
    }

    fn header_bytes(h: PayloadHeader) -> [u8; PAYLOAD_HEADER_LEN] {
        let mut b = [0u8; PAYLOAD_HEADER_LEN];
        b.pwrite_with(h, 0, scroll::LE).expect("fits");
        b
    }

    /// The matching 20-byte section blob (see [`tiny_header`]).
    fn tiny_payload() -> [u8; 20] {
        let mut p = [0u8; 20];
        // zone string-offset table u16[2] = {0, 0}: one empty tzid; the
        // pool at 4 is empty. Grid cell at 16 = NO_ZONE. List offsets
        // u16[1] at 18 = {0}.
        p[16..18].copy_from_slice(&utz_common::NO_ZONE.to_le_bytes());
        p
    }

    #[test]
    fn valid_synthetic_header_parses() {
        let h = parse(&header_bytes(tiny_header())).expect("valid header");
        assert_eq!(h.w_min_e4, 10);
        check_tables(&tiny_payload(), &h).expect("valid tables");
    }

    #[test]
    fn header_field_rejections() {
        for (name, h) in [
            ("w_min over 1.0", {
                let mut h = tiny_header();
                h.w_min_e4 = 10_001;
                h
            }),
            ("reserved nonzero", {
                let mut h = tiny_header();
                h.reserved = 1;
                h
            }),
            ("zero grid columns", {
                let mut h = tiny_header();
                h.ncols = 0;
                h
            }),
            ("non-positive grid_deg", {
                let mut h = tiny_header();
                h.grid_deg = 0.0;
                h
            }),
        ] {
            assert!(
                matches!(parse(&header_bytes(h)), Err(Error::InvalidHeaderField)),
                "{name}"
            );
        }
    }

    #[test]
    fn table_cross_reference_rejections() {
        let h = parse(&header_bytes(tiny_header())).expect("valid header");
        // interior cell naming a feature that doesn't exist
        let mut p = tiny_payload();
        p[16..18].copy_from_slice(&5u16.to_le_bytes());
        assert_eq!(check_tables(&p, &h), Err(Error::TableOutOfRange));
        // border cell naming a candidate list that doesn't exist
        let mut p = tiny_payload();
        p[16..18].copy_from_slice(&0x8000u16.to_le_bytes());
        assert_eq!(check_tables(&p, &h), Err(Error::TableOutOfRange));
        // zone-string offset running past the pool
        let mut p = tiny_payload();
        p[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(check_tables(&p, &h), Err(Error::TableOutOfRange));
    }
}
