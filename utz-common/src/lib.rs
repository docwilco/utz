//! Workspace-shared utilities. Currently: the deterministic LCG behind every
//! reproducible test/bench sampler (previously copy-pasted per crate).
#![no_std]

extern crate alloc;

use alloc::vec::Vec;

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
    #[expect(clippy::cast_precision_loss, reason = "53-bit mantissa construction: state>>11 < 2^53 and 2^53 are both exact")]
    pub fn unit_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// `n` uniform world points `(lon, lat)` from `seed` — the shared sampler
/// behind the measurement commands and benches (same seed → same points, so
/// numbers stay comparable across tools).
#[must_use]
pub fn gen_pts(seed: u64, n: usize) -> Vec<(f64, f64)> {
    let mut lcg = Lcg::new(seed);
    (0..n).map(|_| (lcg.unit_f64() * 360.0 - 180.0, lcg.unit_f64() * 180.0 - 90.0)).collect()
}
