//! The shared μTZ lookup-bench harness. It provides deterministic points,
//! an injected time source (host `Instant` or the firmware timer), and
//! elision-proof results. The crate is `no_std` + `alloc` so the exact same
//! code runs on the CLI and the ESP32-S3 firmware.

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

/// Generates deterministic pseudo-random lon/lat points (from
/// `utz_common::POINT_SEED`, the same seed as the measurement commands,
/// so numbers are comparable).
#[must_use]
pub fn gen_pts(count: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(utz_common::POINT_SEED, count)
}

pub struct BenchResult {
    pub lookups: u32,
    /// The number of points that resolved to a zone (all of them on
    /// with-oceans datasets).
    pub hits: u32,
    pub elapsed_us: u64,
    /// The sum of resolved tzid lengths. It is consumed so the compiler
    /// can't elide the lookups, and it doubles as a cheap cross-platform
    /// answer checksum.
    pub checksum: u64,
}

impl BenchResult {
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/lookup display"
    )]
    pub fn us_per_lookup(&self) -> f64 {
        self.elapsed_us as f64 / f64::from(self.lookups.max(1))
    }
}

/// Runs one timed pass of `finder.lookup()` over `points`. `now_us` is any
/// monotonic microsecond source.
///
/// # Panics
/// If `points` holds more than `u32::MAX` points.
pub fn run(
    finder: &utz::Finder,
    points: &[(f64, f64)],
    now_us: &mut dyn FnMut() -> u64,
) -> BenchResult {
    let (mut hits, mut checksum) = (0u32, 0u64);
    let start_us = now_us();
    for &(lon, lat) in points {
        // gen_pts produces in-range coordinates by construction; the
        // unchecked variant keeps the timed loop on the lookup kernel
        if let Some(tzid) = finder.lookup_unchecked(utz::Position { lon, lat }) {
            hits += 1;
            checksum = checksum.wrapping_add(tzid.len() as u64);
        }
    }
    let elapsed_us = now_us().saturating_sub(start_us);
    BenchResult {
        lookups: u32::try_from(points.len()).expect("point count fits u32"),
        hits,
        elapsed_us,
        checksum,
    }
}

/// Runs one warmup pass plus `rounds` timed passes and returns the fastest
/// round (the steady-state cost, robust against caches warming and
/// interrupt noise).
///
/// # Panics
///
/// This function never panics: `rounds` is clamped to ≥ 1, so a best round
/// always exists.
pub fn run_rounds(
    finder: &utz::Finder,
    points: &[(f64, f64)],
    rounds: usize,
    now_us: &mut dyn FnMut() -> u64,
) -> BenchResult {
    let mut best: Option<BenchResult> = None;
    let _ = run(finder, points, now_us); // warmup
    for _ in 0..rounds.max(1) {
        let round_result = run(finder, points, now_us);
        if best
            .as_ref()
            .is_none_or(|best_so_far| round_result.elapsed_us < best_so_far.elapsed_us)
        {
            best = Some(round_result);
        }
    }
    best.unwrap()
}

/// The build.rs-generated custom shapes both benches embed: they are
/// uncompressed twins of the presets across all geometry encodings (the
/// recipes are in build.rs).
pub mod assets {
    // uncompressed twins of the compact/balanced presets, and tiny-static
    // with fixed-width arcs — the XIP speed tier
    pub static COMPACT_NONE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compact-none.utz"));
    pub static BALANCED_NONE: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/balanced-none.utz"));
    pub static TINY_FIXED: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/tiny-fixed-static.utz"));
    pub static COMPACT_FIXED: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/compact-fixed-none.utz"));
    // full-rings twins need 4-aligned statics (FullRings slice casts)
    pub static TINY_EAGER: &[u8] =
        utz::include_bytes_aligned!(4, concat!(env!("OUT_DIR"), "/tiny-eager-static.utz"));
    pub static COMPACT_EAGER: &[u8] =
        utz::include_bytes_aligned!(4, concat!(env!("OUT_DIR"), "/compact-eager-static.utz"));
    pub static TINY_COARSE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tiny-coarse.utz"));

    // capability guards emitted next to each build.rs asset: a feature
    // mismatch between the recipes and this crate's utz features is a
    // compile error
    include!(concat!(env!("OUT_DIR"), "/compact-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/balanced-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/tiny-fixed-static.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/compact-fixed-none.utz.guard.rs"));
    include!(concat!(env!("OUT_DIR"), "/tiny-eager-static.utz.guard.rs"));
    include!(concat!(
        env!("OUT_DIR"),
        "/compact-eager-static.utz.guard.rs"
    ));
    include!(concat!(env!("OUT_DIR"), "/tiny-coarse.utz.guard.rs"));
}
