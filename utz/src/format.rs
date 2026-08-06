//! The asset container format and its parsing. Using μTZ does not
//! require this module: every [`Finder`](crate::Finder) constructor
//! calls it internally. It is public for the cases where you want to
//! look at an asset without loading it: sniff its provenance (dataset,
//! ε, codec, release) or cheaply validate an OTA download via
//! [`outer()`] and [`parse()`] before committing it to flash.
//!
//! The parser stores OFFSETS into the section blob (no self-referential
//! slices), so the same code serves borrowed (`&'static`, zero-copy) and
//! owned buffers.
//!
//! # Byte-level format (container version 11)
//!
//! This section is normative for [`VERSION`] 11. Every multi-byte
//! integer is little-endian, and nothing is implicitly padded; the one
//! alignment rule is the `FullRings` coordinate section below. A
//! container has three parts: the prologue, the payload header, and the
//! section blob. Only the blob is ever compressed, so an asset is
//! identified and validated before any decompression.
//!
//! ## Prologue (8 bytes, frozen across versions)
//!
//! | offset | size | value                |
//! |-------:|-----:|----------------------|
//! |      0 |    4 | [`MAGIC`], `"uTZ1"`  |
//! |      4 |    1 | [`VERSION`]          |
//! |      5 |    3 | reserved, zero       |
//!
//! ## Payload header (64 bytes, plaintext)
//!
//! The payload header is the serialized [`PayloadHeader`]: its field
//! order is the wire order (the encoder and reader both serialize
//! through the struct, so the two cannot drift; this table spells out
//! the resulting offsets). All `*_off` fields are blob-relative byte
//! offsets; fields the active `geom` does not use are ignored by
//! readers.
//!
//! | offset | type | field |
//! |-------:|------|-------|
//! |      0 | u32  | `arcs_off`: arc store (geom 0/1) / `FullRings` coords (geom 2) |
//! |      4 | u32  | `rings_off`: parent table (+ ring records for geom 0/1) |
//! |      8 | u32  | `grid_off`: grid tables |
//! |     12 | u32  | `release_off`: TZBB release string |
//! |     16 | u32  | `eager_coords`: preload coordinate reservation (Σ referenced-arc vertex counts; exact for geom 2) |
//! |     20 | u32  | `eager_rings`: total ring count |
//! |     24 | u32  | `eager_polys`: total poly count (parent-table length) |
//! |     28 | u32  | `n_arcs`: arc count (geom 0/1; zero otherwise) |
//! |     32 | u32  | `raw_len`: decompressed blob size; every section must end within it |
//! |     36 | f32  | `grid_deg`: cell size in degrees, > 0, may be fractional |
//! |     40 | f32  | `epsilon_m`: simplification ε in meters (provenance) |
//! |     44 | u16  | `n_features`: zone count, < 0x7FFF (15-bit ids) |
//! |     46 | u16  | `ncols` |
//! |     48 | u16  | `nrows` |
//! |     50 | u16  | `uniq`: distinct border-cell candidate lists, ≤ 0x7FFF |
//! |     52 | u16  | `release_len` |
//! |     54 | u16  | `flags`: reserved, must be zero |
//! |     56 | u8   | `dataset`: 0 now / 1 1970 / 2 all; +4 for the land-only variant |
//! |     57 | u8   | `quant_bits`: the width itself, 16 / 24 / 32 |
//! |     58 | u8   | `simplify_algorithm`: 0 none / 1 RDP / 2 Visvalingam / 3 Imai–Iri (provenance) |
//! |     59 | u8   | `geom`: 0 `VarintArcs` / 1 `FixedWidthArcs` / 2 `FullRings` / 3 `Coarse` |
//! |     60 | u8   | `codec`: 0 uncompressed / 1 gzip / 2 zstd / 3 brotli / 4 xz |
//! |     61 | u16  | `density_weight_floor_e4`: weight floor ×1e−4, ≤ 10000 (provenance) |
//! |     63 | u8   | reserved, must be zero |
//!
//! Any other enum byte, nonzero reserved field, `grid_deg` ≤ 0 or NaN,
//! or zero grid dimension makes the container invalid.
//!
//! ## Coordinate quantization
//!
//! With `qmax = 2^(quant_bits−1) − 1`, coordinates quantize as
//! `qlon = round(lon / 180 · qmax)` and `qlat = round(lat / 90 · qmax)`,
//! rounding half away from zero. A stored coordinate is a
//! two's-complement integer of `quant_bits` bits (2 / 3 / 4 bytes,
//! little-endian, sign-extended on read); a vertex is a `(qlon, qlat)`
//! pair of `2 · quant_bits/8` bytes.
//!
//! ## Section blob
//!
//! The blob is compressed as one unit when `codec` ≠ 0 (`raw_len` is
//! the decompressed size); all offsets index the decompressed bytes. The
//! encoder writes sections in this physical order, but readers navigate
//! purely by the header offsets:
//!
//! 1. The **zone string-offset table**, at blob offset 0, holds
//!    `u16 × (n_features+1)` monotone fenceposts. Entries `i` and `i+1`
//!    delimit tzid `i` in the pool; the last entry is the pool's total
//!    length.
//! 2. The **tzid pool**, immediately after (offset `(n_features+1)·2`),
//!    holds the concatenated UTF-8 tzid strings with no separators; the
//!    fencepost offsets are pool-relative. An empty string is a nameless
//!    feature: lookups resolving to it answer `None`.
//! 3. The **geometry sections** follow, per `geom` as described below.
//! 4. The **grid tables** sit at `grid_off`.
//! 5. The **TZBB release string** sits at `release_off` as `release_len`
//!    bytes of UTF-8 (provenance, e.g. `"2026c"`).
//!
//! ### Geometry, geom 0/1 (arc store + ring records)
//!
//! The arc store sits at `arcs_off` and opens with `u32 × (n_arcs+1)`
//! fencepost offsets, relative to the arc data that starts right after
//! the table. Arc `i` occupies `[offset[i], offset[i+1])` and consists
//! of a varint vertex count, the first vertex as an absolute quant-width
//! pair, then per remaining vertex either two zigzag varints (Δlon,
//! Δlat; geom 0) or an absolute quant-width pair (geom 1).
//!
//! The poly tables sit at `rings_off`: the parent table
//! `u16 × eager_polys` (poly id → feature id), then a fencepost table
//! `u32 × (eager_polys+1)` of ring-record offsets relative to the ring
//! data that follows it. A poly's ring record consists of a bbox as 4
//! quant-width coords (min lon, min lat, max lon, max lat), a `u16`
//! ring count, then per ring a varint arc-ref count followed by that
//! many varint arc refs. An arc ref is `arc_id << 1 | reversed`: the
//! ring walks the arc backwards when the low bit is set. Consecutive
//! arcs share their junction vertex and a ring's last vertex repeats
//! its first; readers drop the duplicates while stitching.
//!
//! ### Geometry, geom 2 (`FullRings`)
//!
//! This encoding is the preload cache serialized. It is read in place
//! via slice casts, so it serves little-endian hosts only, and
//! `arcs_off` must be a multiple of 4 (the encoder pads before it;
//! prologue + header = 72 keeps blob alignment in an uncompressed
//! file). Three sections sit back to back starting at `arcs_off`:
//! coordinates as `eager_coords` quant-width pairs (4 / 6 / 8 bytes per
//! vertex), then ring ends `u32 × eager_rings` (cumulative exclusive
//! vertex indices; the last must equal `eager_coords`), then per-poly
//! records of 20 bytes, each a bbox as 4 × `i32` (always full width,
//! unlike the quant-width ring-record bbox) followed by a `u32`
//! exclusive ring-end index. A poly's rings start where the previous
//! poly's ended. Ring closure vertices are already dropped. `rings_off`
//! holds only the parent table; there is no arc store and `n_arcs` is
//! zero.
//!
//! ### Geometry, geom 3 (`Coarse`)
//!
//! A coarse container has no geometry sections. `rings_off` holds the
//! parent table; lookups answer from the grid alone at cell precision.
//!
//! ### Grid tables
//!
//! The primary table sits at `grid_off`: `u16 × ncols·nrows`, row-major
//! with cell (0, 0) at (lon −180°, lat −90°); a query point maps to
//! `col = ⌊(lon + 180) / grid_deg⌋`, `row = ⌊(lat + 90) / grid_deg⌋`,
//! clamped to the grid. Cell values decode as follows:
//!
//! | value | meaning |
//! |-------|---------|
//! | `0x7FFF` | no zone touches the cell |
//! | high bit clear | interior cell: the value is the feature id |
//! | high bit set | border cell: low 15 bits index a candidate list |
//!
//! The candidate lists follow as a CSR pair: `u16 × (uniq+1)` monotone
//! fencepost offsets (counted in entries), then the interned lists
//! themselves as `u16` poly ids. Every list is dominant-first: its head
//! is the poly of the cell's dominant zone, which coarse lookups answer
//! with directly.
//!
//! ## Validation contract
//!
//! [`parse()`] bounds-checks every section end against `raw_len` before
//! any decompression; [`check_tables()`] then validates every
//! cross-reference the lookup path follows without further checks
//! (string offsets, parent entries, grid cell values, candidate lists)
//! in one linear pass, so a corrupt or hostile asset is refused with a
//! typed error instead of panicking a lookup later.

use scroll::Pread;
use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgorithm};
pub use utz_common::{MAGIC, PAYLOAD_HEADER_LEN, PROLOGUE_LEN, VERSION};

use crate::{caps, Error, Result};

/// The parsed header, which records every section position needed for
/// O(1) access.
#[derive(Clone, Copy)]
pub struct PayloadLayout {
    /// The section blob's compression codec.
    pub codec: Codec,
    /// The blob's decompressed size (every stored offset stays within it).
    pub sections_len: usize,
    pub dataset: Dataset,
    pub quant_bits: QuantBits,
    pub geom: GeomEncoding,
    /// A reserved field that must be zero (room for future format flags).
    pub flags: u16,
    /// This field is provenance, not decode logic.
    pub simplify_algorithm: SimplifyAlgorithm,
    /// The cell size in degrees; fractional values (e.g. 0.5) are allowed.
    pub grid_deg: f32,
    pub epsilon_m: f32,
    /// The population-density weight floor ×1e-4 the asset was built with
    /// (0 means unweighted; provenance, exposed as
    /// [`Finder::density_weight_floor()`](crate::Finder::density_weight_floor)).
    pub density_weight_floor_e4: u16,
    pub n_features: u16,
    /// The tzid pool; the zone string-offset table (`u16[n_features+1]`)
    /// starts the section blob at offset 0, and this pool follows it.
    pub pool: usize,
    /// The number of arcs in the arc store.
    pub n_arcs: u32,
    /// The arc offset table, `u32[n_arcs+1]`.
    pub arc_offsets: usize,
    /// The arc coordinate data the offset table indexes.
    pub arc_data: usize,
    /// The parent table mapping poly id → feature id, `u16[eager_polys]`
    /// (grid candidates are polys; per-poly ring records follow).
    pub parent: usize,
    /// The per-poly ring-record offsets, `u32[eager_polys+1]`.
    pub poly_offsets: usize,
    /// The ring records the poly offsets index.
    pub ring_data: usize,
    /// The `FullRings` coordinates (the preload-cache layout
    /// serialized), `(i32, i32)[eager_coords]`, 4-aligned within the
    /// payload (geom=2 only; `usize::MAX` otherwise).
    pub full_coords: usize,
    /// The `FullRings` ring ends, `u32[eager_rings]` (geom=2 only).
    pub full_ring_ends: usize,
    /// The `FullRings` per-poly records, each a bbox `[i32; 4]` followed
    /// by a ring-end `u32` (geom=2 only).
    pub full_polys: usize,
    /// The eager-cache coordinate reservation, the exact `Vec` size for
    /// [`preload()`](crate::Finder::preload) (Σ referenced-arc vertex
    /// counts; it may only over-estimate).
    pub eager_coords: u32,
    /// The eager-cache ring reservation.
    pub eager_rings: u32,
    /// The eager-cache poly reservation.
    pub eager_polys: u32,
    /// The number of grid columns.
    pub ncols: u16,
    /// The number of grid rows.
    pub nrows: u16,
    /// The primary grid table, `u16[ncols*nrows]`.
    pub primary: usize,
    /// The number of distinct border-cell candidate lists.
    pub uniq: u16,
    /// The candidate-list offsets, `u16[uniq+1]`.
    pub list_offsets: usize,
    /// The interned candidate lists the offsets index.
    pub list_ids: usize,
    /// The TZBB release string offset (the payload tail).
    pub release_off: usize,
    /// The TZBB release string length.
    pub release_len: usize,
}

/// Reads the little-endian u16 at `position`.
///
/// # Panics
/// Panics if `position + 2` exceeds `bytes` (all `read_*` helpers assume
/// offsets already validated by [`parse()`]).
#[must_use]
pub fn read_u16(bytes: &[u8], position: usize) -> u16 {
    u16::from_le_bytes([bytes[position], bytes[position + 1]])
}
/// Reads the little-endian u32 at `position`.
///
/// # Panics
/// Panics if `position + 4` exceeds `bytes`.
#[must_use]
pub fn read_u32(bytes: &[u8], position: usize) -> u32 {
    u32::from_le_bytes([
        bytes[position],
        bytes[position + 1],
        bytes[position + 2],
        bytes[position + 3],
    ])
}

/// Reads a fixed-width signed coordinate: 2/3/4 bytes little-endian,
/// sign-extended.
///
/// # Panics
/// Panics if the coordinate's bytes run past `bytes`.
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

/// Reads a varint and returns (value, `next_position`).
///
/// # Panics
/// Panics if the varint runs past `bytes`.
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
/// Undoes zigzag encoding, mapping `0, 1, 2, 3, …` back to
/// `0, -1, 1, -2, …`.
#[must_use]
pub fn unzigzag(value: u64) -> i64 {
    (value >> 1).cast_signed() ^ -((value & 1).cast_signed())
}

/// Validates the prologue (format identity only: magic and version) and
/// returns the payload header's offset.
///
/// # Errors
/// Returns [`Error::Truncated`], [`Error::BadMagic`], or
/// [`Error::UnsupportedVersion`] if the bytes are too short or the
/// magic/version don't match.
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

/// Parses the plaintext payload header. Every section bound is validated
/// against the header's own declared blob size, so a container is fully
/// checked before any decompression happens.
///
/// # Errors
/// Returns [`Error::Truncated`] if `header_bytes` is short,
/// [`Error::InvalidHeaderField`] for invalid header fields (including
/// unknown codec / geometry / quantization / dataset bytes),
/// [`Error::SectionOverrun`] for a section overrunning the blob, and
/// [`Error::GeometryNotCompiledIn`] if the geometry encoding has no
/// compiled-in decoder.
pub fn parse(header_bytes: &[u8]) -> Result<PayloadLayout> {
    // an invalid enum byte (quant_bits/geom/simplify_algorithm/dataset) fails the
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
        simplify_algorithm: header.simplify_algorithm,
        grid_deg: header.grid_deg,
        epsilon_m: header.epsilon_m,
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

/// Validates every cross-reference inside the (decompressed) section
/// tables that [`crate::Finder::lookup()`] follows without further
/// checks: zone-string offsets, the parent table, grid cell values,
/// the candidate lists, and (for `FullRings` assets) the ring-end and
/// per-poly ring tables. The check is one linear pass at load time,
/// so a corrupt or hostile asset is refused with a typed error instead
/// of panicking a lookup later.
///
/// # Errors
/// Returns [`Error::TableOutOfRange`] on the first out-of-range
/// reference.
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
    // FullRings ring tables: lookups subslice the coordinate section by
    // these values with unchecked length math (a wrapped range there is
    // UB, not a panic), so both tables must be monotone and bounded —
    // ring ends within the coordinate count, per-poly ring ends within
    // the ring count. A range's start is the previous entry's end, so
    // monotonicity bounds both ends of every derived range.
    if layout.geom == GeomEncoding::FullRings {
        let mut previous_end = 0u32;
        for i in 0..layout.eager_rings as usize {
            let end = read_u32(payload, layout.full_ring_ends + i * 4);
            if end < previous_end || end > layout.eager_coords {
                return Err(out_of_range);
            }
            previous_end = end;
        }
        let mut previous_end = 0u32;
        for i in 0..poly_count {
            let end = read_u32(payload, layout.full_polys + i * 20 + 16);
            if end < previous_end || end > layout.eager_rings {
                return Err(out_of_range);
            }
            previous_end = end;
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
        let in_range = match utz_common::CellTag::from_cell(cell) {
            utz_common::CellTag::Empty => true,
            utz_common::CellTag::Zone(zone) => usize::from(zone) < feature_count,
            utz_common::CellTag::Border(list_index) => usize::from(list_index) < uniq,
        };
        if !in_range {
            return Err(out_of_range);
        }
    }
    Ok(())
}

/// Rejects a section end past the blob end.
fn need(sections_len: usize, end: usize) -> Result<()> {
    if sections_len < end {
        Err(Error::SectionOverrun)
    } else {
        Ok(())
    }
}

/// The geometry-dependent section offsets for [`PayloadLayout`]; the
/// fields a given encoding doesn't use stay at
/// [`GeometrySections::NONE`]'s markers.
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
    /// No sections are present: zero arcs, and every offset is
    /// `usize::MAX`.
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
/// parent table and the grid.
fn coarse_sections(sections_len: usize, parent: usize, n_polys: usize) -> Result<GeometrySections> {
    need(sections_len, parent + n_polys * 2)?;
    Ok(GeometrySections::NONE)
}

/// `FullRings` (geom 2) stores the preload-cache layout in place of the
/// arc store and ring records. Coordinates must be 4-aligned within the
/// payload (the encoder pads; the 12-byte outer header preserves it in
/// flash).
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

/// The arc-store encodings (geom 0/1) carry the arc offset table and
/// data, plus the per-poly ring records.
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

/// Returns the TZBB release string recorded in the header.
#[must_use]
pub fn release<'p>(layout: &PayloadLayout, payload: &'p [u8]) -> &'p [u8] {
    &payload[layout.release_off..layout.release_off + layout.release_len]
}

#[cfg(all(test, feature = "geom-varint-arcs"))]
mod tests {
    use super::*;
    use scroll::Pwrite;
    use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgorithm};

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
            epsilon_m: 500.0,
            n_features: 1,
            ncols: 1,
            nrows: 1,
            uniq: 0,
            release_len: 0,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits24,
            simplify_algorithm: SimplifyAlgorithm::Rdp,
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

    /// The [`tiny_header`] sibling for `FullRings` (geom 2): one feature
    /// whose single poly has two rings over six i32 coordinate pairs.
    /// Sections: zone table 0..4, coords 4..52, ring ends 52..60, the
    /// poly record 60..80 (bbox + ring end), parent 80..82, grid 82..84,
    /// list offsets 84..86.
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_header() -> PayloadHeader {
        let mut header = tiny_header();
        header.geom = GeomEncoding::FullRings;
        header.quant_bits = QuantBits::Bits32;
        header.arcs_off = 4;
        header.rings_off = 80;
        header.grid_off = 82;
        header.release_off = 86;
        header.raw_len = 86;
        header.eager_coords = 6;
        header.eager_rings = 2;
        header.eager_polys = 1;
        header
    }

    /// The matching section blob (see [`full_rings_header`]): coordinate
    /// values are irrelevant to the table checks and stay zero, as does
    /// the parent entry (feature 0) and the poly bbox.
    #[cfg(feature = "geom-full-rings")]
    fn full_rings_payload() -> [u8; 86] {
        let mut payload = [0u8; 86];
        // ring ends {3, 6}; the poly record's ring end is 2
        payload[52..56].copy_from_slice(&3u32.to_le_bytes());
        payload[56..60].copy_from_slice(&6u32.to_le_bytes());
        payload[76..80].copy_from_slice(&2u32.to_le_bytes());
        payload[82..84].copy_from_slice(&utz_common::NO_ZONE.to_le_bytes());
        payload
    }

    /// The ring tables feed unchecked slice-length math in the lookup
    /// fold, so [`check_tables()`] must prove them monotone and bounded at
    /// load: a non-monotone ring-end table is exactly the input that
    /// would otherwise wrap the fold's coordinate-count subtraction.
    #[cfg(feature = "geom-full-rings")]
    #[test]
    fn full_rings_ring_table_rejections() {
        let layout = parse(&header_bytes(full_rings_header())).expect("valid header");
        check_tables(&full_rings_payload(), &layout).expect("valid tables");
        // non-monotone ring ends {5, 3}
        let mut payload = full_rings_payload();
        payload[52..56].copy_from_slice(&5u32.to_le_bytes());
        payload[56..60].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
        // a ring end past the coordinate count
        let mut payload = full_rings_payload();
        payload[56..60].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
        // the poly record's ring end past the ring count
        let mut payload = full_rings_payload();
        payload[76..80].copy_from_slice(&3u32.to_le_bytes());
        assert_eq!(check_tables(&payload, &layout), Err(Error::TableOutOfRange));
    }
}
