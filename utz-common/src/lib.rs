//! Workspace-shared vocabulary and utilities: the container codec
//! identifiers, the payload header record both the encoder and the reader
//! serialize through, and the deterministic LCG behind every reproducible
//! test/bench sampler (previously copy-pasted per crate).
#![no_std]

use scroll::{Pread, Pwrite, SizeWith};

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

// on-disk magic stays ASCII ("μ" is 2 bytes in UTF-8 and byte literals
// reject non-ASCII); the project brands as μTZ, the container as uTZ1
pub const MAGIC: [u8; 4] = *b"uTZ1";
pub const VERSION: u8 = 8; // v8: one fixed payload-header record (all counts
                           // hoisted, release string at the payload tail);
                           // v7 flags byte + image coords at quant width;
                           // v6 12-byte outer + EagerImage; v5 bbox;
                           // v4 poly grid; v3 geom

/// [`PayloadHeader`]'s serialized size — the zone table starts here.
pub const PAYLOAD_HEADER_LEN: usize = 56;

/// The payload's one fixed header record — everything the reader needs to
/// locate every section. The encoder `Pwrite`s it, the reader `Pread`s it
/// (both little-endian); field order is the wire order.
///
/// Layout after the header: zone-string offsets + pool, the
/// geometry-dependent sections at the stored offsets, the grid tables, and
/// the TZBB release string at `release_off`.
#[derive(Debug, Clone, Copy, PartialEq, Pread, Pwrite, SizeWith)]
pub struct PayloadHeader {
    /// arc store (geom 0/1) / `EagerImage` coords (geom 2, 4-aligned)
    pub arcs_off: u32,
    /// poly→feature parent table (+ ring records for geom 0/1)
    pub rings_off: u32,
    /// grid tables: primary cells, then CSR list offsets + ids
    pub grid_off: u32,
    /// TZBB release string (`release_len` bytes at the payload tail)
    pub release_off: u32,
    /// eager-cache reservation counts: exact Vec sizes for `preload`
    /// (coords is Σ referenced-arc vcounts — may only over-estimate)
    pub eager_coords: u32,
    pub eager_rings: u32,
    pub eager_polys: u32,
    /// arc count (geom 0/1; zero when there is no arc store)
    pub n_arcs: u32,
    /// grid cell size in degrees — fractional (e.g. 0.5) allowed
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
}

/// Implement the scroll wire traits for a single-byte header type with
/// `byte()`/`from_byte()`: an invalid byte fails the header read itself.
macro_rules! wire_byte {
    ($ty:ty, $invalid:literal) => {
        impl scroll::ctx::TryFromCtx<'_, scroll::Endian> for $ty {
            type Error = scroll::Error;
            fn try_from_ctx(
                source: &[u8],
                ctx: scroll::Endian,
            ) -> Result<(Self, usize), scroll::Error> {
                let byte: u8 = source.pread_with(0, ctx)?;
                let value = <$ty>::from_byte(byte).ok_or(scroll::Error::BadInput {
                    size: 1,
                    msg: $invalid,
                })?;
                Ok((value, 1))
            }
        }
        impl scroll::ctx::TryIntoCtx<scroll::Endian> for $ty {
            type Error = scroll::Error;
            fn try_into_ctx(
                self,
                target: &mut [u8],
                ctx: scroll::Endian,
            ) -> Result<usize, scroll::Error> {
                target.pwrite_with(self.byte(), 0, ctx)
            }
        }
        impl scroll::ctx::TryIntoCtx<scroll::Endian> for &$ty {
            type Error = scroll::Error;
            fn try_into_ctx(
                self,
                target: &mut [u8],
                ctx: scroll::Endian,
            ) -> Result<usize, scroll::Error> {
                (*self).try_into_ctx(target, ctx)
            }
        }
        impl scroll::ctx::SizeWith<scroll::Endian> for $ty {
            fn size_with(_: &scroll::Endian) -> usize {
                1
            }
        }
    };
}

/// Coordinate quantization width: how many bits each stored coordinate
/// occupies (the wire value IS the bit count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QuantBits {
    Bits16 = 16,
    Bits24 = 24,
    Bits32 = 32,
}

impl QuantBits {
    /// The width's header byte (= the bit count).
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }

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

wire_byte!(QuantBits, "invalid quant_bits header byte");

/// Geometry encoding, recorded in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum GeomEncoding {
    /// Shared arcs as delta + zigzag-varint streams — the default, for
    /// minimal storage size.
    #[default]
    DeltaVarint = 0,
    /// Shared arcs as absolute fixed-width coordinates. Costs storage —
    /// raw arcs grow 40–72% and the best-compressed container 24–32%
    /// (xz overtakes brotli) — but streaming lookups no longer decode a
    /// varint per vertex, the dominant lookup cost on embedded targets.
    /// Near-eager speed with no RAM cache, suited to uncompressed
    /// `-static` assets read in place from memory-mapped flash.
    Fixed = 1,
    /// The geometry section is the preload cache itself: coordinates
    /// flattened per ring as `(i32, i32)` runs plus the ring/poly index
    /// tables, 4-byte aligned. The slice kernels read it directly —
    /// from memory-mapped flash via `from_static` (eager-lookup speed
    /// with no RAM cache and no preload pass at boot) or from the
    /// decompressed buffer via `from_slice`. There is no arc store, so
    /// arcs shared between zones are duplicated per ring: raw size is
    /// ~4.1–4.3× the varint payload and the best-compressed container
    /// grows 61–94% (xz).
    EagerImage = 2,
    /// Grid-only asset: header, tzid pool, parent table, and grid — no
    /// geometry at all. `lookup()` answers at cell precision, the same
    /// answer `lookup_coarse` gives; precision is a property of the
    /// asset, like `eps_m`. By far the smallest storage (about a third
    /// of even the varint payload for the tiny preset), works on any
    /// endianness, and a reader built only for coarse assets compiles
    /// no point-in-polygon code.
    Coarse = 3,
}

impl GeomEncoding {
    /// The encoding's header byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// The encoding a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<GeomEncoding> {
        match byte {
            0 => Some(GeomEncoding::DeltaVarint),
            1 => Some(GeomEncoding::Fixed),
            2 => Some(GeomEncoding::EagerImage),
            3 => Some(GeomEncoding::Coarse),
            _ => None,
        }
    }
}

wire_byte!(GeomEncoding, "invalid geometry-encoding header byte");

/// Simplification algorithm recorded in the header — provenance, not decode
/// logic. RDP is the default; Imai–Iri gives provably minimum vertices for
/// the same ε bound (slower encode). Visvalingam has an area knob, not ε.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SimplifyAlgo {
    #[default]
    Rdp = 0,
    Visvalingam = 1,
    ImaiIri = 2,
    /// No simplification — geometry stored as sourced.
    None = 3,
}

impl SimplifyAlgo {
    /// The algorithm's header byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// The algorithm a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<SimplifyAlgo> {
        match byte {
            0 => Some(SimplifyAlgo::Rdp),
            1 => Some(SimplifyAlgo::Visvalingam),
            2 => Some(SimplifyAlgo::ImaiIri),
            3 => Some(SimplifyAlgo::None),
            _ => None,
        }
    }
}

wire_byte!(SimplifyAlgo, "invalid simplify-algorithm header byte");

/// TZBB vintage a container was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Vintage {
    /// zones distinct today
    Now = 0,
    /// zones distinct since 1970
    Since1970 = 1,
    /// every distinct tzid
    All = 2,
}

/// The dataset byte: vintage in bits 0–1, bit 2 set = land-only
/// (clear = with oceans).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dataset {
    pub vintage: Vintage,
    pub land_only: bool,
}

impl Dataset {
    /// The dataset's header byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self.vintage as u8 | if self.land_only { 4 } else { 0 }
    }

    /// The dataset a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Dataset> {
        let vintage = match byte & 0b11 {
            0 => Vintage::Now,
            1 => Vintage::Since1970,
            2 => Vintage::All,
            _ => return None,
        };
        if byte & !0b111 != 0 {
            return None; // reserved bits set
        }
        Some(Dataset {
            vintage,
            land_only: byte & 4 != 0,
        })
    }
}

wire_byte!(Dataset, "invalid dataset header byte");

/// A container's payload codec — the outer header's codec byte, shared
/// between the encoder (which picks one) and the reader (which dispatches
/// on it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Codec {
    Uncompressed = 0,
    Gzip = 1,
    Zstd = 2,
    Brotli = 3,
    Xz = 4,
}

impl Codec {
    /// The codec's outer-header byte.
    #[must_use]
    pub const fn byte(self) -> u8 {
        self as u8
    }

    /// The codec a header byte names, if any.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Codec> {
        match byte {
            0 => Some(Codec::Uncompressed),
            1 => Some(Codec::Gzip),
            2 => Some(Codec::Zstd),
            3 => Some(Codec::Brotli),
            4 => Some(Codec::Xz),
            _ => None,
        }
    }
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

/// `n` uniform world points `(lon, lat)` from `seed` — the shared sampler
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
        Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo, Vintage,
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
            grid_deg: 0.5,
            eps_m: 50.0,
            n_features: 9,
            ncols: 10,
            nrows: 11,
            uniq: 12,
            release_len: 13,
            flags: 0,
            dataset: Dataset {
                vintage: Vintage::Now,
                land_only: false,
            },
            quant_bits: QuantBits::Bits24,
            simplify_algo: SimplifyAlgo::Rdp,
            geom: GeomEncoding::Fixed,
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
    fn codec_bytes_round_trip() {
        for codec in [
            Codec::Uncompressed,
            Codec::Gzip,
            Codec::Zstd,
            Codec::Brotli,
            Codec::Xz,
        ] {
            assert_eq!(Codec::from_byte(codec.byte()), Some(codec));
        }
    }
}
