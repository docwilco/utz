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

use crate::{caps, Error, Result};

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
    pub density_weight_floor_e4: u16,
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

/// Little-endian u16 at `position`.
///
/// # Panics
/// If `position + 2` exceeds `bytes` (all `read_*` helpers assume offsets
/// already validated by [`parse()`]).
#[must_use]
pub fn read_u16(bytes: &[u8], position: usize) -> u16 {
    u16::from_le_bytes([bytes[position], bytes[position + 1]])
}
/// Little-endian u32 at `position`.
///
/// # Panics
/// If `position + 4` exceeds `bytes`.
#[must_use]
pub fn read_u32(bytes: &[u8], position: usize) -> u32 {
    u32::from_le_bytes([
        bytes[position],
        bytes[position + 1],
        bytes[position + 2],
        bytes[position + 3],
    ])
}

/// Fixed-width signed coord: 2/3/4 bytes little-endian, sign-extended.
///
/// # Panics
/// If the coord's bytes run past `bytes`.
#[must_use]
pub fn read_fixed(bytes: &[u8], position: usize, quant_bits: QuantBits) -> i32 {
    match quant_bits {
        QuantBits::Bits16 => i32::from(read_u16(bytes, position).cast_signed()),
        QuantBits::Bits24 => {
            let value = i32::from(bytes[position])
                | (i32::from(bytes[position + 1]) << 8)
                | (i32::from(bytes[position + 2]) << 16);
            if value & 0x0080_0000 != 0 {
                value | !0x00FF_FFFF
            } else {
                value
            }
        }
        QuantBits::Bits32 => read_u32(bytes, position).cast_signed(),
    }
}

/// Varint; returns (value, `next_position`).
///
/// # Panics
/// If the varint runs past `bytes`.
#[must_use]
pub fn read_varint(bytes: &[u8], mut position: usize) -> (u64, usize) {
    let (mut value, mut shift) = (0u64, 0u32);
    loop {
        let byte = bytes[position];
        position += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return (value, position);
        }
        shift += 7;
    }
}
/// Undo zigzag encoding: map `0, 1, 2, 3, …` back to `0, -1, 1, -2, …`.
#[must_use]
pub fn unzigzag(value: u64) -> i64 {
    (value >> 1).cast_signed() ^ -((value & 1).cast_signed())
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
/// [`Error::Truncated`] if `header_bytes` is short;
/// [`Error::InvalidHeaderField`] for invalid header fields (including
/// unknown codec / geometry / quantization / dataset bytes);
/// [`Error::SectionOverrun`] for a section overrunning the blob;
/// [`Error::GeometryNotCompiledIn`] if the geometry encoding has no
/// compiled-in decoder.
pub fn parse(header_bytes: &[u8]) -> Result<PayloadLayout> {
    // an invalid enum byte (quant_bits/geom/simplify_algo/dataset) fails the
    // header read itself as BadInput; running out of bytes means the source
    // ends inside the header
    let header: PayloadHeader =
        header_bytes
            .pread_with(0, scroll::LE)
            .map_err(|source| match source {
                scroll::Error::BadInput { .. } => Error::InvalidHeaderField,
                _ => Error::Truncated,
            })?;
    if header.flags != 0
        || header.reserved != 0
        || header.density_weight_floor_e4 > 10_000
        || header.grid_deg.is_nan()
        || header.grid_deg <= 0.0
        || header.ncols == 0
        || header.nrows == 0
    {
        return Err(Error::InvalidHeaderField);
    }
    let sections_len = header.raw_len as usize;
    // a valid geom byte whose decoder isn't compiled in is refused loudly
    if !caps::geom_compiled_in(header.geom) {
        return Err(Error::GeometryNotCompiledIn(header.geom));
    }

    // The tzid pool sits right after the zone string-offset table at blob
    // offset 0: n_features+1 fencepost entries (last one marks the end of
    // the final string), 2 bytes each.
    let pool = (header.n_features as usize + 1) * 2;

    let n_polys = header.eager_polys as usize;
    let (arcs_off, parent) = (header.arcs_off as usize, header.rings_off as usize);
    let sections = match header.geom {
        GeomEncoding::Coarse => coarse_sections(sections_len, parent, n_polys)?,
        GeomEncoding::FullRings => full_rings_sections(
            sections_len,
            header.quant_bits,
            arcs_off,
            n_polys,
            header.eager_coords,
            header.eager_rings,
        )?,
        GeomEncoding::VarintArcs | GeomEncoding::FixedWidthArcs => {
            arc_sections(sections_len, arcs_off, parent, n_polys, header.n_arcs)?
        }
    };

    let primary = header.grid_off as usize;
    let list_offsets = primary + header.ncols as usize * header.nrows as usize * 2;
    let list_ids = list_offsets + (header.uniq as usize + 1) * 2;
    need(sections_len, list_ids)?;
    let (release_off, release_len) = (header.release_off as usize, header.release_len as usize);
    need(sections_len, release_off + release_len)?;

    Ok(PayloadLayout {
        codec: header.codec,
        sections_len,
        dataset: header.dataset,
        quant_bits: header.quant_bits,
        geom: header.geom,
        flags: header.flags,
        simplify_algo: header.simplify_algo,
        grid_deg: header.grid_deg,
        eps_m: header.eps_m,
        density_weight_floor_e4: header.density_weight_floor_e4,
        n_features: header.n_features,
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
        eager_coords: header.eager_coords,
        eager_rings: header.eager_rings,
        eager_polys: header.eager_polys,
        ncols: header.ncols,
        nrows: header.nrows,
        primary,
        uniq: header.uniq,
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
pub fn check_tables(payload: &[u8], layout: &PayloadLayout) -> Result<()> {
    let out_of_range = Error::TableOutOfRange;
    let feature_count = usize::from(layout.n_features);
    // zone-string offset table: u16[n_features+1] at 0, monotone, within
    // the payload once rebased onto the pool
    let mut previous_offset = 0u16;
    for i in 0..=feature_count {
        let offset = crate::format::read_u16(payload, i * 2);
        if offset < previous_offset || layout.pool + usize::from(offset) > layout.sections_len {
            return Err(out_of_range);
        }
        previous_offset = offset;
    }
    // parent table: poly id → feature id
    let poly_count = layout.eager_polys as usize;
    for i in 0..poly_count {
        if usize::from(read_u16(payload, layout.parent + i * 2)) >= feature_count {
            return Err(out_of_range);
        }
    }
    // candidate lists: u16[uniq+1] offsets monotone and in range, ids are
    // poly ids
    let uniq = usize::from(layout.uniq);
    let list_len = (layout.release_off.saturating_sub(layout.list_ids)) / 2;
    let mut previous_offset = 0u16;
    for i in 0..=uniq {
        let offset = read_u16(payload, layout.list_offsets + i * 2);
        if offset < previous_offset || usize::from(offset) > list_len {
            return Err(out_of_range);
        }
        previous_offset = offset;
    }
    let end = usize::from(previous_offset);
    for i in 0..end {
        if usize::from(read_u16(payload, layout.list_ids + i * 2)) >= poly_count {
            return Err(out_of_range);
        }
    }
    // grid: interior cells name a feature, border cells a candidate list
    for i in 0..(layout.ncols as usize * layout.nrows as usize) {
        let cell = read_u16(payload, layout.primary + i * 2);
        if cell == utz_common::NO_ZONE {
            continue;
        }
        if cell & 0x8000 == 0 {
            if usize::from(cell) >= feature_count {
                return Err(out_of_range);
            }
        } else if usize::from(cell & 0x7FFF) >= uniq {
            return Err(out_of_range);
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
pub fn release<'p>(layout: &PayloadLayout, payload: &'p [u8]) -> &'p [u8] {
    &payload[layout.release_off..layout.release_off + layout.release_len]
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
            density_weight_floor_e4: 10,
            reserved: 0,
        }
    }

    fn header_bytes(header: PayloadHeader) -> [u8; PAYLOAD_HEADER_LEN] {
        let mut bytes = [0u8; PAYLOAD_HEADER_LEN];
        bytes.pwrite_with(header, 0, scroll::LE).expect("fits");
        bytes
    }

    /// The matching 20-byte section blob (see [`tiny_header`]).
    fn tiny_payload() -> [u8; 20] {
        let mut payload = [0u8; 20];
        // zone string-offset table u16[2] = {0, 0}: one empty tzid; the
        // pool at 4 is empty. Grid cell at 16 = NO_ZONE. List offsets
        // u16[1] at 18 = {0}.
        payload[16..18].copy_from_slice(&utz_common::NO_ZONE.to_le_bytes());
        payload
    }

    #[test]
    fn valid_synthetic_header_parses() {
        let layout = parse(&header_bytes(tiny_header())).expect("valid header");
        assert_eq!(layout.density_weight_floor_e4, 10);
        check_tables(&tiny_payload(), &layout).expect("valid tables");
    }

    #[test]
    fn header_field_rejections() {
        for (name, header) in [
            ("density weight floor over 1.0", {
                let mut header = tiny_header();
                header.density_weight_floor_e4 = 10_001;
                header
            }),
            ("reserved nonzero", {
                let mut header = tiny_header();
                header.reserved = 1;
                header
            }),
            ("zero grid columns", {
                let mut header = tiny_header();
                header.ncols = 0;
                header
            }),
            ("non-positive grid_deg", {
                let mut header = tiny_header();
                header.grid_deg = 0.0;
                header
            }),
        ] {
            assert!(
                matches!(parse(&header_bytes(header)), Err(Error::InvalidHeaderField)),
                "{name}"
            );
        }
    }

    #[test]
    fn table_cross_reference_rejections() {
        let layout = parse(&header_bytes(tiny_header())).expect("valid header");
        // interior cell naming a feature that doesn't exist
        let mut payload = tiny_payload();
        payload[16..18].copy_from_slice(&5u16.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
        // border cell naming a candidate list that doesn't exist
        let mut payload = tiny_payload();
        payload[16..18].copy_from_slice(&0x8000u16.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
        // zone-string offset running past the pool
        let mut payload = tiny_payload();
        payload[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
    }
}
