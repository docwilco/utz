//! This crate holds the types and utilities shared across the workspace:
//! the asset codec identifiers, the payload header record that both the
//! encoder and the reader serialize through, and the deterministic LCG
//! behind every reproducible test/bench sampler.
#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]

use scroll::{Pread, Pwrite};

pub mod presets;

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// The magic bytes every `.utz` asset starts with. The bytes are ASCII on
/// purpose ("μ" is 2 bytes in UTF-8 and byte literals reject non-ASCII):
/// the project brands itself as μTZ, and the container as uTZ1.
pub const MAGIC: [u8; 4] = *b"uTZ1";
/// The container-format version this workspace reads and writes; a reader
/// refuses any other value.
pub const VERSION: u8 = 11;

/// The byte length of the container prologue, which is `MAGIC` (4),
/// `VERSION` (1), and 3 reserved bytes. The prologue is the only part of
/// the format whose layout is frozen across versions; everything after it
/// is [`VERSION`]-specific.
pub const PROLOGUE_LEN: usize = 8;

/// [`PayloadHeader`]'s serialized size. The section blob starts at
/// `PROLOGUE_LEN + PAYLOAD_HEADER_LEN`.
pub const PAYLOAD_HEADER_LEN: usize = 64;

/// The primary grid table's "no zone covers this cell" marker (all 15 id
/// bits set). Zone/feature ids stay strictly below it; the u16's high bit
/// flags a border cell carrying a candidate-list index instead.
pub const NO_ZONE: u16 = 0x7FFF;

/// A primary-grid cell's border flag: the u16's high bit set means the
/// low 15 bits index a candidate list instead of naming a zone.
pub const BORDER_FLAG: u16 = 0x8000;

/// The mask selecting a primary-grid cell's 15-bit value, a zone id or
/// (under [`BORDER_FLAG`]) a candidate-list index. Zone ids and list
/// indices stay strictly below [`NO_ZONE`], the all-bits-set sentinel.
pub const CELL_ID_MASK: u16 = 0x7FFF;

/// One decoded primary-grid cell. The wire form is the u16 the
/// constants above describe; this decoder is shared by the runtime
/// lookup, the format validator, and the encoder's measurement tools,
/// so the tag layout is interpreted in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTag {
    /// No zone covers the cell.
    Empty,
    /// A single zone covers the cell; a lookup answers without any
    /// point-in-polygon test.
    Zone(u16),
    /// A zone boundary passes through the cell; the value indexes the
    /// cell's candidate list.
    Border(u16),
}

impl CellTag {
    /// Decodes a primary-table cell value.
    #[must_use]
    pub const fn from_cell(cell: u16) -> CellTag {
        if cell & BORDER_FLAG != 0 {
            CellTag::Border(cell & CELL_ID_MASK)
        } else if cell == NO_ZONE {
            CellTag::Empty
        } else {
            CellTag::Zone(cell)
        }
    }
}

/// The half-range of an `i{bits}` quantization grid (`2^(bits-1) - 1`).
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "qmax = 2^(bits-1)-1 ≤ 2^31-1, exact in f64"
)]
pub const fn qmax_for(bits: u32) -> f64 {
    ((1u64 << (bits - 1)) - 1) as f64
}

/// Rounds half away from zero via the truncating cast: `core` has no
/// `f64::round`, and the encoder shares this exact expression with the
/// reader's lookup quantizer so the two stay bit-identical.
#[expect(
    clippy::cast_possible_truncation,
    reason = "callers bound |value| below i32::MAX; float as saturates"
)]
fn round_q(value: f64) -> i32 {
    (value + if value >= 0.0 { 0.5 } else { -0.5 }) as i32
}

/// Quantizes a longitude onto a grid with half-range `qmax` (see
/// [`qmax_for()`]).
#[must_use]
pub fn q_lon(lon: f64, qmax: f64) -> i32 {
    round_q(lon / 180.0 * qmax)
}

/// Quantizes a latitude onto a grid with half-range `qmax`.
#[must_use]
pub fn q_lat(lat: f64, qmax: f64) -> i32 {
    round_q(lat / 90.0 * qmax)
}

/// Dequantizes a longitude from a grid with half-range `qmax`, the
/// inverse of [`q_lon()`]. The input is `f64` so fractional grid
/// positions (e.g. ring intersection points) dequantize too; stored
/// coordinates pass through `f64::from`.
#[must_use]
pub fn dq_lon(value: f64, qmax: f64) -> f64 {
    value / qmax * 180.0
}

/// Dequantizes a latitude from a grid with half-range `qmax`, the
/// inverse of [`q_lat()`].
#[must_use]
pub fn dq_lat(value: f64, qmax: f64) -> f64 {
    value / qmax * 90.0
}

/// The primary-grid cell containing a lon/lat position, as `(row, col)`:
/// truncate toward zero, then clamp into the grid. The encoder's
/// rasterizer, the runtime reader, and the measurement tools all share
/// this one formula, so what a lookup probes is exactly what the grid
/// indexed.
#[must_use]
pub fn grid_cell(lon: f64, lat: f64, deg: f64, ncols: usize, nrows: usize) -> (usize, usize) {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "cast saturates, then the clamp keeps the index in grid range"
    )]
    let cell = (
        (((lat + 90.0) / deg) as i64).clamp(0, nrows as i64 - 1) as usize,
        (((lon + 180.0) / deg) as i64).clamp(0, ncols as i64 - 1) as usize,
    );
    cell
}

/// The container's one fixed header record, which carries everything the
/// reader needs to locate every section. It sits in PLAINTEXT right after
/// the outer header (only the section blob after it is compressed), so
/// any container is inspectable and validated before decompression. The
/// encoder `Pwrite`s it, the reader `Pread`s it (both little-endian);
/// field order is the wire order; all offsets are relative to the section
/// blob that follows.
///
/// The section blob is laid out as the zone-string offsets and pool, the
/// geometry-dependent sections at the stored offsets, the grid tables, and
/// the TZBB release string at `release_off`.
#[derive(Debug, Clone, Copy, PartialEq, Pread, Pwrite)]
pub struct PayloadHeader {
    /// The offset of the arc store (geom 0/1) or of the `FullRings`
    /// coordinates (geom 2, 4-aligned).
    pub arcs_off: u32,
    /// The offset of the poly→feature parent table (plus the ring records
    /// for geom 0/1).
    pub rings_off: u32,
    /// The offset of the grid tables, which hold the primary cells
    /// followed by the CSR list offsets and ids.
    pub grid_off: u32,
    /// The offset of the TZBB release string (`release_len` bytes at the
    /// payload tail).
    pub release_off: u32,
    /// The eager-cache reservation counts, which give exact Vec sizes for
    /// `preload()` (the coords count is Σ referenced-arc vertex counts and
    /// may only over-estimate).
    pub eager_coords: u32,
    pub eager_rings: u32,
    pub eager_polys: u32,
    /// The arc count (geom 0/1), which is zero when there is no arc store.
    pub n_arcs: u32,
    /// The section blob's decompressed size (so readers allocate once).
    pub raw_len: u32,
    /// The grid cell size in degrees; fractional sizes (e.g. 0.5) are
    /// allowed.
    pub grid_deg: f32,
    /// The simplification tolerance the asset was built with (provenance).
    pub epsilon_m: f32,
    pub n_features: u16,
    /// The grid's column count.
    pub ncols: u16,
    /// The grid's row count.
    pub nrows: u16,
    /// The number of distinct border-cell candidate lists (CSR).
    pub uniq: u16,
    pub release_len: u16,
    /// This field is reserved and must be zero (room for future format
    /// flags).
    pub flags: u16,
    pub dataset: Dataset,
    pub quant_bits: QuantBits,
    pub simplify_algorithm: SimplifyAlgorithm,
    pub geom: GeomEncoding,
    /// The section blob's compression codec.
    pub codec: Codec,
    /// The population-density weight floor in fixed-point 1e-4, where 0
    /// means unweighted and 10000 means 1.0. Like `epsilon_m`, it is
    /// provenance for the density-weighted simplification knob.
    pub density_weight_floor_e4: u16,
    /// This byte is reserved and must be zero (it pads the header to 64
    /// bytes).
    pub reserved: u8,
}

impl PayloadHeader {
    /// Parses the plaintext header out of a complete asset, checking the
    /// prologue first (magic + [`VERSION`]). The result is `None` if the
    /// bytes are too short, not a μTZ asset, or a different format
    /// version. The parse reads only the first
    /// `PROLOGUE_LEN + PAYLOAD_HEADER_LEN` bytes, which makes it cheap
    /// provenance sniffing without decompression.
    #[must_use]
    pub fn from_asset(bytes: &[u8]) -> Option<PayloadHeader> {
        if bytes.len() < PROLOGUE_LEN + PAYLOAD_HEADER_LEN
            || bytes[..4] != MAGIC
            || bytes[4] != VERSION
        {
            return None;
        }
        bytes.pread_with(PROLOGUE_LEN, scroll::LE).ok()
    }
}

/// The coordinate quantization width, which is how many bits each stored
/// coordinate occupies (the wire value IS the bit count).
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

    /// The number of bytes per stored coordinate (2 / 3 / 4).
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

/// The geometry encoding, which decides how an asset stores its polygons
/// and how lookups read them.
///
/// The three polygon encodings answer bit-identically; they trade storage
/// for lookup speed. `Coarse` alone trades precision instead: it drops the
/// polygons and answers at grid-cell precision.
///
/// The measured cost/speed ladder lives here. Every other doc site links
/// back to this table rather than restating numbers. Size columns are
/// whole assets relative to the `VarintArcs` build of the same preset;
/// lookup speed is the flash-XIP (execute-in-place, zero RAM) leg of the
/// embedded bench, against the same baseline.
///
/// | encoding         | geometry section                          |        raw | best-compressed | XIP lookup      |
/// |------------------|-------------------------------------------|-----------:|----------------:|-----------------|
/// | `VarintArcs`     | shared arcs, delta + zigzag-varint coords |         1× |              1× | 1×              |
/// | `FixedWidthArcs` | shared arcs, absolute quant-width coords  |   1.4–1.7× |   1.15–1.3× (xz) | 1.3–1.5×        |
/// | `FullRings`      | whole rings, absolute quant-width coords  |   2.1–3.2× |    1.4–2.1× (xz) | 2.0–3.3×        |
/// | `Coarse`         | none (grid only)                          | 0.05–0.33× |     0.01–0.14× | grid probe only |
///
/// Narrow-quant `FullRings` XIP even outran the RAM preload cache on the
/// embedded bench. On fixed-width payloads the codec ranking flips: xz
/// overtakes brotli at every preset. `Coarse` shrinks with preset fineness
/// twice over: the grid is all it keeps, and grids compress extremely well.
///
/// The size columns come from `utz_dev_cli whittle --extended` (2026-07,
/// TZBB 2026c), measuring full assets per preset recipe against the
/// `VarintArcs` build. The XIP
/// lookup column is from the 2026-07 bench-firmware runs against earlier
/// payload revisions; read it as a relative ladder.
// TODO(verify): re-run the XIP lookup column on target against the
// current payload revisions (last measured 2026-07 on earlier ones).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pread, Pwrite)]
#[repr(u8)]
pub enum GeomEncoding {
    /// This encoding stores shared arcs as delta + zigzag-varint streams;
    /// it is the default, for minimal storage size.
    #[default]
    VarintArcs = 0,
    /// This encoding stores shared arcs as absolute fixed-width
    /// (quant-width) coordinates, so streaming lookups no longer decode a
    /// varint per vertex, the dominant lookup cost on embedded targets. It
    /// reaches near-eager speed with no RAM cache and suits uncompressed
    /// `-static` assets read in place from memory-mapped flash, at a
    /// storage cost (table above).
    FixedWidthArcs = 1,
    /// This encoding stores each ring in full: coordinates are flattened
    /// per ring as quant-width pairs plus ring/poly index tables, 4-byte
    /// aligned; it is the preload cache serialized. Slice kernels read it
    /// in place: from memory-mapped flash via `from_static()`
    /// (eager-lookup speed with no RAM cache and no preload pass at boot)
    /// or from the decompressed buffer via `from_slice()`. There is no arc
    /// store, so borders shared between zones are duplicated per ring,
    /// making this the largest encoding (table above). It works on
    /// little-endian hosts only.
    FullRings = 2,
    /// A grid-only asset carrying the header, tzid pool, parent table,
    /// and grid, with no geometry at all. `lookup()` answers at cell
    /// precision, the same answer `lookup_coarse()` gives; precision is a
    /// property of the asset, like `epsilon_m`. It is by far the smallest
    /// storage (table above) and works on any endianness, and a reader
    /// built only for coarse assets compiles no point-in-polygon code.
    Coarse = 3,
}

#[cfg_attr(
    on_docsrs,
    doc = "[`utz_simplify`]: https://docs.rs/utz_simplify/latest/utz_simplify/index.html"
)]
#[cfg_attr(on_docsrs, doc = "")]
/// The simplification algorithm records which simplifier the encoder ran
/// when the asset was generated. Each variant sketches its algorithm;
/// guarantees, citations, and the density-weighted variants are
/// documented in the [`utz_simplify`] crate.
///
/// RDP is the default because I had the best results with it when combined with
/// the population density weighting. But your mileage may vary, so I kept the
/// algorithms in for people to play with.
///
/// [`utz_simplify`]: ../utz_simplify/index.html
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Pread, Pwrite)]
#[repr(u8)]
pub enum SimplifyAlgorithm {
    /// No simplification runs; the geometry is stored as sourced.
    None = 0,
    /// Ramer–Douglas–Peucker keeps every point within a maximum deviation
    /// of ε. This is the default.
    #[default]
    Rdp = 1,
    /// Visvalingam–Whyatt repeatedly drops the vertex spanning the
    /// smallest triangle, trading RDP's deviation bound for a
    /// cartographically smoother caricature; the ε-driven pipeline
    /// derives its area threshold as ε², matching the viewer.
    Visvalingam = 2,
    /// Imai–Iri produces provably minimum vertices for the same ε bound
    /// as RDP, at a slower encode. Measured against RDP on full-planet
    /// arcs it keeps 3.8% fewer vertices at ε = 100 m, rising to 18–19%
    /// fewer at ε = 500–2000 m; a full-pipeline check at ε = 2000 m
    /// produced a 12.6% smaller compressed asset for about four seconds
    /// of extra encode time.
    ImaiIri = 3,
}

impl SimplifyAlgorithm {
    /// The algorithm a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<SimplifyAlgorithm> {
        match byte {
            0 => Some(SimplifyAlgorithm::None),
            1 => Some(SimplifyAlgorithm::Rdp),
            2 => Some(SimplifyAlgorithm::Visvalingam),
            3 => Some(SimplifyAlgorithm::ImaiIri),
            _ => None,
        }
    }
}

/// The dataset an asset was built from, naming a
/// [timezone-boundary-builder] release as a zone set × ocean coverage
/// pair. The zone set trades zone count for historical fidelity: `now`
/// and `1970` merge zones whose rules are identical since that date,
/// and `all` keeps every distinct tzid. Ocean coverage decides whether
/// maritime timezones tile the rest of the planet or queries at sea
/// return `None`. (The discriminants keep the wire bitfield: the zone
/// set lives in bits 0–1, and a set bit 2 means land-only.)
///
/// [timezone-boundary-builder]: https://github.com/evansiroky/timezone-boundary-builder
#[derive(Debug, Clone, Copy, PartialEq, Eq, Pread, Pwrite)]
#[repr(u8)]
pub enum Dataset {
    /// Zones whose rules are identical from today onward are merged and
    /// the oceans are covered. This is the smallest set, and the preset
    /// default.
    Now = 0,
    /// Zones identical since 1970 are merged and the oceans are covered,
    /// which is enough to resolve any post-1970 civil time correctly.
    Since1970 = 1,
    /// Every distinct tzid is kept and the oceans are covered, giving
    /// full historical fidelity; this is the largest set.
    All = 2,
    /// This is `Now` without maritime zones, so queries at sea return
    /// `None`.
    NowLandOnly = 4,
    /// This is `Since1970` without maritime zones.
    Since1970LandOnly = 5,
    /// This is `All` without maritime zones.
    AllLandOnly = 6,
}

impl Dataset {
    /// Returns the canonical dataset name (`now`, `1970`, or `all`, with
    /// a `land-` prefix for the land-only variants), the spelling the
    /// builder's dataset parser accepts.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Dataset::Now => "now",
            Dataset::Since1970 => "1970",
            Dataset::All => "all",
            Dataset::NowLandOnly => "land-now",
            Dataset::Since1970LandOnly => "land-1970",
            Dataset::AllLandOnly => "land-all",
        }
    }

    /// The zone-set half of the name (`"now"`, `"1970"`, or `"all"`),
    /// which picks how aggressively zones with identical rules are
    /// merged.
    #[must_use]
    pub const fn zone_set(self) -> &'static str {
        match self {
            Dataset::Now | Dataset::NowLandOnly => "now",
            Dataset::Since1970 | Dataset::Since1970LandOnly => "1970",
            Dataset::All | Dataset::AllLandOnly => "all",
        }
    }

    /// Whether maritime timezones cover the oceans (`false` names the
    /// `land-` releases).
    #[must_use]
    pub const fn oceans(self) -> bool {
        matches!(self, Dataset::Now | Dataset::Since1970 | Dataset::All)
    }

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

/// The compression applied to an asset's payload. The encoder picks one
/// codec; loading the asset needs the matching decoder compiled in.
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

/// A minimal MMIX LCG for reproducible test/bench data (not for
/// cryptography). Callers keep their own seeds so historical sequences
/// stay bit-identical.
pub struct Lcg(pub u64);

impl Lcg {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Advances and returns the full 64-bit state.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(MMIX_MUL).wrapping_add(MMIX_ADD);
        self.0
    }

    /// Returns a uniform value in [0, 1) via a 53-bit mantissa
    /// construction.
    #[expect(
        clippy::cast_precision_loss,
        reason = "53-bit mantissa construction: state>>11 < 2^53 and 2^53 are both exact"
    )]
    pub fn unit_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The workspace-wide seed for `gen_pts()` point sampling. Every bench
/// and measurement command draws the same point sequence from it, so
/// lookup numbers stay comparable across tools.
pub const POINT_SEED: u64 = 0x1234_5678;

/// The workspace-wide seed for pseudo-random payload bytes in codec
/// roundtrip tests (ASCII `uztrip`).
pub const PAYLOAD_SEED: u64 = 0x757a_7472_6970;

/// Generates `count` uniform world points `(lon, lat)` from `seed`. This
/// is the shared sampler behind the measurement commands and benches: the
/// same seed gives the same points, so numbers stay comparable across
/// tools.
#[cfg(feature = "alloc")]
#[must_use]
pub fn gen_pts(seed: u64, count: usize) -> Vec<(f64, f64)> {
    let mut lcg = Lcg::new(seed);
    (0..count)
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
        Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgorithm,
        PAYLOAD_HEADER_LEN,
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
            epsilon_m: 50.0,
            n_features: 9,
            ncols: 10,
            nrows: 11,
            uniq: 12,
            release_len: 13,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits24,
            simplify_algorithm: SimplifyAlgorithm::Rdp,
            geom: GeomEncoding::FixedWidthArcs,
            codec: Codec::Brotli,
            density_weight_floor_e4: 200,
            reserved: 0,
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
            epsilon_m: 0.0,
            n_features: 0,
            ncols: 0,
            nrows: 0,
            uniq: 0,
            release_len: 0,
            flags: 0,
            dataset: Dataset::Now,
            quant_bits: QuantBits::Bits16,
            simplify_algorithm: SimplifyAlgorithm::None,
            geom: GeomEncoding::VarintArcs,
            codec: Codec::Uncompressed,
            density_weight_floor_e4: 0,
            reserved: 0,
        };
        let mut bytes = [0u8; PAYLOAD_HEADER_LEN];
        bytes
            .pwrite_with(header, 0, LE)
            .expect("buffer sized to the declared length");
        bytes[57] = 17; // quant_bits: no such width
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
        bytes[57] = 16;
        bytes[56] = 3; // dataset: zone-set code 3 is unassigned
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
        bytes[56] = 0;
        bytes[60] = 9; // codec: no such codec
        assert!(bytes.pread_with::<PayloadHeader>(0, LE).is_err());
    }
}
