//! Host-side μTZ lookup bench: same harness (utz-bench-common) and the same
//! deterministic points as the ESP32-S3 firmware, so host and target numbers
//! (and answer checksums) are directly comparable.
//!
//!     cargo run --release -p utz-bench-cli -- <container.utz> [npts] [rounds]

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;

#[derive(Parser)]
#[command(name = "utz-bench-cli", about = "μTZ lookup benchmark over a .utz container")]
struct Args {
    /// container file (make one: cargo run --release -p utz-build -- encode now 500)
    container: PathBuf,
    /// number of uniform lon/lat sample points
    #[arg(default_value_t = 100_000)]
    npts: usize,
    /// timed rounds (fastest wins; one untimed warmup pass first)
    #[arg(default_value_t = 5)]
    rounds: usize,
}

fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let bytes = std::fs::read(&a.container)?;
    let size = bytes.len();
    let finder = utz::Finder::from_vec(bytes).map_err(|e| anyhow::anyhow!("decode: {e}"))?;
    println!(
        "{}: {:.1} KiB on disk, tzbb release {:?}",
        a.container.display(),
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
