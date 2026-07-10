//! Shared `μTZ` lookup-bench harness: deterministic points, an injected time
//! source (host `Instant` / firmware timer), and elision-proof results.
//! `no_std` + `alloc` so the exact same code runs on the CLI and the
//! ESP32-S3 firmware.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Deterministic pseudo-random lon/lat points (same LCG recipe as the
/// utz-build measurement commands, so numbers are comparable).
#[must_use]
pub fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    let mut lcg = 0x1234_5678u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
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
    #[must_use]
    pub fn us_per_lookup(&self) -> f64 {
        self.elapsed_us as f64 / f64::from(self.lookups.max(1))
    }
}

/// Run one timed pass of `finder.lookup` over `pts`. `now_us` is any
/// monotonic microsecond source.
///
/// # Panics
/// If `pts` holds more than `u32::MAX` points.
pub fn run(finder: &utz::Finder, pts: &[(f64, f64)], now_us: &mut dyn FnMut() -> u64) -> BenchResult {
    let (mut hits, mut checksum) = (0u32, 0u64);
    let t0 = now_us();
    for &(lon, lat) in pts {
        if let Some(tz) = finder.lookup(utz::Position { lon, lat }) {
            hits += 1;
            checksum = checksum.wrapping_add(tz.len() as u64);
        }
    }
    let elapsed_us = now_us().saturating_sub(t0);
    BenchResult { lookups: u32::try_from(pts.len()).expect("point count fits u32"), hits, elapsed_us, checksum }
}

/// `warmup` + `rounds` passes; returns the fastest round (steady-state cost,
/// robust against caches warming and interrupt noise).
///
/// # Panics
///
/// Never: `rounds` is clamped to ≥ 1, so a best round always exists.
pub fn run_rounds(finder: &utz::Finder, pts: &[(f64, f64)], rounds: usize, now_us: &mut dyn FnMut() -> u64) -> BenchResult {
    let mut best: Option<BenchResult> = None;
    let _ = run(finder, pts, now_us); // warmup
    for _ in 0..rounds.max(1) {
        let r = run(finder, pts, now_us);
        if best.as_ref().is_none_or(|b| r.elapsed_us < b.elapsed_us) {
            best = Some(r);
        }
    }
    best.unwrap()
}

/// The build.rs-generated custom shapes both benches embed: uncompressed
/// twins of the presets across all geometry encodings (recipes in build.rs).
pub mod assets {
    // uncompressed twins of the compact/balanced presets, and tiny-static
    // with fixed-width arcs — the XIP speed tier
    pub static COMPACT_NONE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compact-none.utz"));
    pub static BALANCED_NONE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/balanced-none.utz"));
    pub static TINY_FIXED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tiny-fixed-static.utz"));
    pub static COMPACT_FIXED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compact-fixed-none.utz"));
    // eager-image twins need 4-aligned statics (EagerImage slice casts)
    pub static TINY_EAGER: &[u8] = utz::include_bytes_aligned!(4, concat!(env!("OUT_DIR"), "/tiny-eager-static.utz"));
    pub static COMPACT_EAGER: &[u8] = utz::include_bytes_aligned!(4, concat!(env!("OUT_DIR"), "/compact-eager-static.utz"));
    pub static TINY_COARSE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tiny-coarse.utz"));

    // capability guards emitted next to each build.rs asset: a feature
    // mismatch between the recipes and this crate's utz features is a
    // compile error
    include!(concat!(env!("OUT_DIR"), "/compact-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/balanced-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/tiny-fixed-static.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/compact-fixed-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/tiny-eager-static.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/compact-eager-static.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/tiny-coarse.utz.guard.rs"));
}
