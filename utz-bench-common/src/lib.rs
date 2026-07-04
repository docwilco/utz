//! Shared μTZ lookup-bench harness: deterministic points, an injected time
//! source (host `Instant` / firmware timer), and elision-proof results.
//! `no_std` + `alloc` so the exact same code runs on the CLI and the
//! ESP32-S3 firmware.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Deterministic pseudo-random lon/lat points (same LCG recipe as the
/// utz-build measurement commands, so numbers are comparable).
pub fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (lcg >> 11) as f64 / (1u64 << 53) as f64
    };
    (0..n).map(|_| (next() * 360.0 - 180.0, next() * 180.0 - 90.0)).collect()
}

pub struct BenchResult {
    pub lookups: u32,
    /// points that resolved to a zone (all of them on with-oceans datasets)
    pub hits: u32,
    pub elapsed_us: u64,
    /// sum of resolved tzid lengths — consumed so the compiler can't elide
    /// the lookups; also a cheap cross-platform answer checksum
    pub checksum: u64,
}

impl BenchResult {
    pub fn us_per_lookup(&self) -> f64 {
        self.elapsed_us as f64 / self.lookups.max(1) as f64
    }
}

/// Run one timed pass of `finder.lookup` over `pts`. `now_us` is any
/// monotonic microsecond source.
pub fn run(finder: &utz::Finder, pts: &[(f64, f64)], now_us: &mut dyn FnMut() -> u64) -> BenchResult {
    let (mut hits, mut checksum) = (0u32, 0u64);
    let t0 = now_us();
    for &(lon, lat) in pts {
        if let Some(tz) = finder.lookup(lon, lat) {
            hits += 1;
            checksum = checksum.wrapping_add(tz.len() as u64);
        }
    }
    let elapsed_us = now_us().saturating_sub(t0);
    BenchResult { lookups: pts.len() as u32, hits, elapsed_us, checksum }
}

/// `warmup` + `rounds` passes; returns the fastest round (steady-state cost,
/// robust against caches warming and interrupt noise).
pub fn run_rounds(finder: &utz::Finder, pts: &[(f64, f64)], rounds: usize, now_us: &mut dyn FnMut() -> u64) -> BenchResult {
    let mut best: Option<BenchResult> = None;
    let _ = run(finder, pts, now_us); // warmup
    for _ in 0..rounds.max(1) {
        let r = run(finder, pts, now_us);
        if best.as_ref().map(|b| r.elapsed_us < b.elapsed_us).unwrap_or(true) {
            best = Some(r);
        }
    }
    best.unwrap()
}
