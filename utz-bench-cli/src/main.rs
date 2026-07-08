//! Host-side μTZ lookup bench: same harness (utz-bench-common) and the same
//! deterministic points as the ESP32-S3 firmware, so host and target numbers
//! (and answer checksums) are directly comparable.
//!
//!     cargo run --release -p utz-bench-cli -- <shape|container.utz> [npts] [rounds]
//!
//! A shape name picks an embedded container: the presets (`tiny`,
//! `tiny-static`, `compact`, `balanced`, `accurate` — the utz-data-* crates,
//! via the `utz` preset features) or a build.rs-generated custom shape
//! (`compact-none`, `balanced-none`, and `tiny-fixed-static` — tiny-static
//! with fixed-width arcs, the XIP speed tier). Anything else is read as a
//! `.utz` file path.

use std::time::Instant;

use clap::Parser;

// uncompressed twins of the compact/balanced presets, and tiny-static with
// fixed-width arcs — the XIP speed tier (see build.rs)
static COMPACT_NONE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compact-none.utz"));
static BALANCED_NONE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/balanced-none.utz"));
static TINY_FIXED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tiny-fixed-static.utz"));
static COMPACT_FIXED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compact-fixed-none.utz"));

/// The embedded container for a shape name, if the argument is one.
fn embedded(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "tiny" => utz::data::TINY,
        "tiny-static" => utz::data::TINY_STATIC,
        "tiny-fixed-static" => TINY_FIXED,
        "compact" => utz::data::COMPACT,
        "compact-none" => COMPACT_NONE,
        "compact-fixed-none" => COMPACT_FIXED,
        "balanced" => utz::data::BALANCED,
        "balanced-none" => BALANCED_NONE,
        "accurate" => utz::data::ACCURATE,
        _ => return None,
    })
}

#[derive(Parser)]
#[command(name = "utz-bench-cli", about = "μTZ lookup benchmark over a preset shape or .utz container")]
struct Args {
    /// shape name (tiny, tiny-static, tiny-fixed-static, compact,
    /// compact-none, compact-fixed-none, balanced, balanced-none, accurate)
    /// or a container path
    container: String,
    /// number of uniform lon/lat sample points
    #[arg(default_value_t = 100_000)]
    npts: usize,
    /// timed rounds (fastest wins; one untimed warmup pass first)
    #[arg(default_value_t = 5)]
    rounds: usize,
}

fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let bytes = match embedded(&a.container) {
        Some(b) => b.to_vec(),
        None => std::fs::read(&a.container)?,
    };
    let size = bytes.len();
    let finder = utz::Finder::from_vec(bytes).map_err(|e| anyhow::anyhow!("decode: {e}"))?;
    println!(
        "{}: {:.1} KiB container, tzbb release {:?}",
        a.container,
        size as f64 / 1024.0,
        finder.tzbb_release()
    );

    let pts = utz_bench_common::gen_pts(a.npts);
    let t0 = Instant::now();
    let mut now_us = move || t0.elapsed().as_micros() as u64;
    let r = utz_bench_common::run_rounds(&finder, &pts, a.rounds, &mut now_us);
    println!(
        "{} lookups · {} hits · {} µs · {:.3} µs/lookup · {:.0} lookups/s · checksum {}",
        r.lookups,
        r.hits,
        r.elapsed_us,
        r.us_per_lookup(),
        1e6 / r.us_per_lookup(),
        r.checksum
    );
    Ok(())
}
