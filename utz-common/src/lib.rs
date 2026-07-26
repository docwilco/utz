//! Types and utilities shared across the workspace: the container codec
//! identifiers, the payload header record both the encoder and the reader
//! serialize through, and the deterministic LCG behind every reproducible
//! test/bench sampler.
#![no_std]

use scroll::{Pread, Pwrite};

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

// on-disk magic stays ASCII ("μ" is 2 bytes in UTF-8 and byte literals
// reject non-ASCII); the project brands as μTZ, the container as uTZ1
pub const MAGIC: [u8; 4] = *b"uTZ1";
pub const VERSION: u8 = 10;

/// The container prologue: `MAGIC` (4), `VERSION` (1), 3 reserved bytes.
/// The only part of the format whose layout is frozen across versions:
/// everything after it is [`VERSION`]-specific.
pub const PROLOGUE_LEN: usize = 8;

/// [`PayloadHeader`]'s serialized size: the section blob starts at
/// `PROLOGUE_LEN + PAYLOAD_HEADER_LEN`.
pub const PAYLOAD_HEADER_LEN: usize = 64;

/// The primary grid table's "no zone covers this cell" marker (all 15 id
/// bits set). Zone/feature ids stay strictly below it; the u16's high bit
/// flags a border cell carrying a candidate-list index instead.
pub const NO_ZONE: u16 = 0x7FFF;

/// The container's one fixed header record: everything the reader needs to
/// locate every section. It sits in PLAINTEXT right after the outer header
/// (only the section blob after it is compressed), so any container is
/// inspectable and validated before decompression. The encoder `Pwrite`s
/// it, the reader `Pread`s it (both little-endian); field order is the wire
/// order; all offsets are relative to the section blob that follows.
///
/// Section blob layout: zone-string offsets + pool, the geometry-dependent
/// sections at the stored offsets, the grid tables, and the TZBB release
/// string at `release_off`.
#[derive(Debug, Clone, Copy, PartialEq, Pread, Pwrite)]
pub struct PayloadHeader {
    /// arc store (geom 0/1) / `FullRings` coords (geom 2, 4-aligned)
    pub arcs_off: u32,
    /// poly→feature parent table (+ ring records for geom 0/1)
    pub rings_off: u32,
    /// grid tables: primary cells, then CSR list offsets + ids
    pub grid_off: u32,
    /// TZBB release string (`release_len` bytes at the payload tail)
    pub release_off: u32,
    /// eager-cache reservation counts: exact Vec sizes for `preload`
    /// (coords is Σ referenced-arc vcounts; may only over-estimate)
    pub eager_coords: u32,
    pub eager_rings: u32,
    pub eager_polys: u32,
    /// arc count (geom 0/1; zero when there is no arc store)
    pub n_arcs: u32,
    /// the section blob's decompressed size (so readers allocate once)
    pub raw_len: u32,
    /// grid cell size in degrees; fractional (e.g. 0.5) allowed
    pub grid_deg: f32,
    /// simplification tolerance the asset was built with (provenance)
    pub eps_m: f32,
    pub n_features: u16,
    /// grid dimensions
    pub ncols: u16,
    pub nrows: u16,
    /// distinct border-cell candidate lists (CSR)
    pub uniq: u16,
    pub release_len: u16,
    /// reserved, must be zero (room for future format flags)
    pub flags: u16,
    pub dataset: Dataset,
    pub quant_bits: QuantBits,
    pub simplify_algo: SimplifyAlgo,
    pub geom: GeomEncoding,
    /// the section blob's compression codec
    pub codec: Codec,
    /// reserved, must be zero (pads the header to 64 bytes)
    pub reserved: [u8; 3],
}

/// Coordinate quantization width: how many bits each stored coordinate
/// occupies (the wire value IS the bit count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pread, Pwrite)]
#[repr(u8)]
pub enum QuantBits {
    Bits16 = 16,
    Bits24 = 24,
    Bits32 = 32,
}

impl QuantBits {
    /// The width in bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self as u32
    }

    /// Bytes per stored coordinate (2 / 3 / 4).
    #[must_use]
    pub const fn bytes(self) -> usize {
        (self as usize) / 8
    }

    /// The width a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<QuantBits> {
        match byte {
            16 => Some(QuantBits::Bits16),
            24 => Some(QuantBits::Bits24),
            32 => Some(QuantBits::Bits32),
            _ => None,
        }
    }
}

/// Geometry encoding, recorded in the header: what the geometry section
/// contains and how lookups read it.
///
/// The three polygon encodings answer bit-identically; they trade storage
/// for lookup speed. `Coarse` alone trades precision instead: it drops the
/// polygons and answers at grid-cell precision.
///
/// The measured cost/speed ladder lives here. Every other doc site links
/// back to this table rather than restating numbers. Size columns are
/// whole containers relative to the `VarintArcs` build of the same preset;
/// lookup speed is the flash-XIP (execute-in-place, zero RAM) leg of the
/// embedded bench, same baseline.
///
/// | encoding         | geometry section                          |      raw | best-compressed | XIP lookup      |
/// |------------------|-------------------------------------------|---------:|----------------:|-----------------|
/// | `VarintArcs`     | shared arcs, delta + zigzag-varint coords |       1× |              1× | 1×              |
/// | `FixedWidthArcs` | shared arcs, absolute quant-width coords  |  +40–72% |    +24–32% (xz) | 1.3–1.5×        |
/// | `FullRings`      | whole rings, absolute quant-width coords  | 2.1–3.2× |    +61–94% (xz) | 2.0–3.3×        |
/// | `Coarse`         | none (grid only)                          |      ~⅓× |             ~⅓× | grid probe only |
///
/// Narrow-quant `FullRings` XIP even outran the RAM preload cache on the
/// embedded bench. On fixed-width payloads the codec ranking flips: xz
/// overtakes brotli at every shape.
///
/// **TODO(verify):** these figures date to the 2026-07 `fixedwidth_size` /
/// `imagepack_size` sweeps and bench-firmware runs on earlier payload
/// revisions; re-run them against the current format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pread, Pwrite)]
#[repr(u8)]
pub enum GeomEncoding {
    /// Shared arcs as delta + zigzag-varint streams: the default, for
    /// minimal storage size.
    #[default]
    VarintArcs = 0,
    /// Shared arcs as absolute fixed-width (quant-width) coordinates:
    /// streaming lookups no longer decode a varint per vertex, the
    /// dominant lookup cost on embedded targets. Near-eager speed with no
    /// RAM cache, suited to uncompressed `-static` assets read in place
    /// from memory-mapped flash. Costs storage (table above).
    FixedWidthArcs = 1,
    /// Each ring stored in full: coordinates flattened per ring as
    /// quant-width pairs plus ring/poly index tables, 4-byte aligned; the
    /// preload cache serialized. Slice kernels read it in place: from
    /// memory-mapped flash via `from_static` (eager-lookup speed with no
    /// RAM cache and no preload pass at boot) or from the decompressed
    /// buffer via `from_slice`. There is no arc store, so borders shared
    /// between zones are duplicated per ring, the largest encoding
    /// (table above). Little-endian hosts only.
    FullRings = 2,
    /// Grid-only asset: header, tzid pool, parent table, and grid — no
    /// geometry at all. `lookup()` answers at cell precision, the same
    /// answer `lookup_coarse` gives; precision is a property of the
    /// asset, like `eps_m`. By far the smallest storage (table above),
    /// works on any endianness, and a reader built only for coarse
    /// assets compiles no point-in-polygon code.
    Coarse = 3,
}

/// Simplification algorithm: selects the simplifier the encoder runs, and
/// is recorded in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pread, Pwrite)]
#[repr(u8)]
pub enum SimplifyAlgo {
    /// No simplification: geometry stored as sourced.
    None = 0,
    /// Ramer–Douglas–Peucker: keeps every point within a maximum deviation
    /// of ε. The default.
    #[default]
    Rdp = 1,
    /// Visvalingam–Whyatt: repeatedly drops the vertex spanning the
    /// smallest triangle; the ε-driven pipeline derives its area threshold
    /// as ε², matching the viewer.
    Visvalingam = 2,
    /// Imai–Iri: provably minimum vertices for the same ε bound as RDP,
    /// at a slower encode.
    ImaiIri = 3,
}

impl SimplifyAlgo {
    /// The algorithm a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<SimplifyAlgo> {
        match byte {
            0 => Some(SimplifyAlgo::None),
            1 => Some(SimplifyAlgo::Rdp),
            2 => Some(SimplifyAlgo::Visvalingam),
            3 => Some(SimplifyAlgo::ImaiIri),
            _ => None,
        }
    }
}

/// The dataset a container was built from: TZBB vintage × coverage.
/// Discriminants keep the wire bitfield: vintage in bits 0–1, bit 2 set =
/// land-only (clear = with oceans).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pread, Pwrite)]
#[repr(u8)]
pub enum Dataset {
    /// zones distinct today, with oceans
    Now = 0,
    /// zones distinct since 1970, with oceans
    Since1970 = 1,
    /// every distinct tzid, with oceans
    All = 2,
    /// zones distinct today, land only
    NowLandOnly = 4,
    /// zones distinct since 1970, land only
    Since1970LandOnly = 5,
    /// every distinct tzid, land only
    AllLandOnly = 6,
}

impl Dataset {
    /// The dataset a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Dataset> {
        match byte {
            0 => Some(Dataset::Now),
            1 => Some(Dataset::Since1970),
            2 => Some(Dataset::All),
            4 => Some(Dataset::NowLandOnly),
            5 => Some(Dataset::Since1970LandOnly),
            6 => Some(Dataset::AllLandOnly),
            _ => None,
        }
    }
}

/// A container's payload codec: the outer header's codec byte, shared
/// between the encoder (which picks one) and the reader (which dispatches
/// on it).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Pread, Pwrite)]
#[repr(u8)]
pub enum Codec {
    Uncompressed = 0,
    Gzip = 1,
    Zstd = 2,
    Brotli = 3,
    Xz = 4,
}

/// Knuth's MMIX LCG multiplier (TAOCP vol. 2, 3rd ed., §3.3.4 table 1).
pub const MMIX_MUL: u64 = 0x5851_F42D_4C95_7F2D; // 6_364_136_223_846_793_005
/// Knuth's MMIX LCG increment.
pub const MMIX_ADD: u64 = 0x1405_7B7E_F767_814F; // 1_442_695_040_888_963_407

/// Minimal MMIX LCG for reproducible test/bench data (not for cryptography).
/// Callers keep their own seeds so historical sequences stay bit-identical.
pub struct Lcg(pub u64);

impl Lcg {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Advance and return the full 64-bit state.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(MMIX_MUL).wrapping_add(MMIX_ADD);
        self.0
    }

    /// Uniform in [0, 1): 53-bit mantissa construction.
    #[expect(
        clippy::cast_precision_loss,
        reason = "53-bit mantissa construction: state>>11 < 2^53 and 2^53 are both exact"
    )]
    pub fn unit_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// `n` uniform world points `(lon, lat)` from `seed`: the shared sampler
/// behind the measurement commands and benches (same seed → same points, so
/// numbers stay comparable across tools).
#[cfg(feature = "alloc")]
#[must_use]
pub fn gen_pts(seed: u64, n: usize) -> Vec<(f64, f64)> {
    let mut lcg = Lcg::new(seed);
    (0..n)
        .map(|_| {
            (
                lcg.unit_f64() * 360.0 - 180.0,
                lcg.unit_f64() * 180.0 - 90.0,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use scroll::{Pread, Pwrite, LE};

    use super::{
        Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo, PAYLOAD_HEADER_LEN,
    };

    #[test]
    fn payload_header_round_trips_at_declared_length() {
        let header = PayloadHeader {
            arcs_off: 1,
            rings_off: 2,
            grid_off: 3,
            release_off: 4,
            eager_coords: 5,
            eager_rings: 6,
            eager_polys: 7,
            n_arcs: 8,
            raw_len: 14,
            grid_deg: 0.5,
            eps_m: 50.0,
            n_features: 9,
            ncols: 10,
            nrows: 11,
            uniq: 12,
            release_len: 13,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits24,
            simplify_algo: SimplifyAlgo::Rdp,
            geom: GeomEncoding::FixedWidthArcs,
            codec: Codec::Brotli,
            reserved: [0; 3],
        };
        let mut bytes = [0u8; PAYLOAD_HEADER_LEN];
        let written = bytes
            .pwrite_with(header, 0, LE)
            .expect("buffer sized to the declared length");
        assert_eq!(written, PAYLOAD_HEADER_LEN);
        let read: PayloadHeader = bytes
            .pread_with(0, LE)
            .expect("round-trip read of a full buffer");
        assert_eq!(read, header);
    }

    #[test]
    fn invalid_header_byte_fails_the_read() {
        let header = PayloadHeader {
            arcs_off: 0,
            rings_off: 0,
            grid_off: 0,
            release_off: 0,
            eager_coords: 0,
            eager_rings: 0,
            eager_polys: 0,
            n_arcs: 0,
            raw_len: 0,
            grid_deg: 1.0,
            eps_m: 0.0,
            n_features: 0,
            ncols: 0,
            nrows: 0,
            uniq: 0,
            release_len: 0,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits16,
            simplify_algo: SimplifyAlgo::None,
            geom: GeomEncoding::VarintArcs,
            codec: Codec::Uncompressed,
            reserved: [0; 3],
        };
        let mut bytes = [0u8; PAYLOAD_HEADER_LEN];
        bytes
            .pwrite_with(header, 0, LE)
            .expect("buffer sized to the declared length");
        bytes[57] = 17; // quant_bits: no such width
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
        bytes[57] = 16;
        bytes[56] = 3; // dataset: vintage 3 is unassigned
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
        bytes[56] = 0;
        bytes[60] = 9; // codec: no such codec
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
    }
}
