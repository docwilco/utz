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
    pub dataset: u8,
    /// coordinate quantization width: 16, 24, or 32 bits
    pub quant_bits: u8,
    /// simplification algorithm (provenance): 0 = RDP, 1 = Visvalingam,
    /// 2 = Imai–Iri
    pub simplify_algo: u8,
    /// geometry encoding: 0 = delta+varint arcs, 1 = fixed-width arcs,
    /// 2 = `EagerImage`, 3 = coarse (grid-only)
    pub geom: u8,
}

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

    use super::{Codec, PayloadHeader, PAYLOAD_HEADER_LEN};

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
            dataset: 14,
            quant_bits: 24,
            simplify_algo: 0,
            geom: 1,
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
