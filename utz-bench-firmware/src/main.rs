//! μTZ lookup bench on ESP32-S3: embeds an *uncompressed* container in flash,
//! borrows it zero-copy via `Finder::from_static` (XIP — the payload is never
//! copied to RAM), and runs the same harness + points as utz-bench-cli, so
//! the checksums must match the host run exactly.
//!
//! Setup (once): see README.md. Then `cargo run --release` flashes + monitors.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::main;
use esp_hal::time::Instant;
use esp_println::println;

// generate + place with:
//   cargo run --release -p utz-build -- encode now 500 --codec none -o utz-bench-firmware/container.utz
static CONTAINER: &[u8] = include_bytes!("../container.utz");

/// modest by host standards; the S3 has 512 KiB SRAM and f64 PIP is soft-float
const NPTS: usize = 2_000;
const ROUNDS: usize = 3;

#[main]
fn main() -> ! {
    let _peripherals = esp_hal::init(esp_hal::Config::default());
    // Finder scratch + the points vec; the container itself stays in flash
    esp_alloc::heap_allocator!(size: 128 * 1024);

    println!("uTZ bench on ESP32-S3 — container {} KiB in flash", CONTAINER.len() / 1024);
    let finder = utz::Finder::from_static(CONTAINER).expect("container decode");
    println!("tzbb release: {:?}", finder.tzbb_release());

    let pts = utz_bench_common::gen_pts(NPTS);
    let mut now_us = || Instant::now().duration_since_epoch().as_micros();

    loop {
        let r = utz_bench_common::run_rounds(&finder, &pts, ROUNDS, &mut now_us);
        println!(
            "{} lookups · {} hits · {} us · {} us/lookup · checksum {}",
            r.lookups,
            r.hits,
            r.elapsed_us,
            r.elapsed_us / r.lookups as u64,
            r.checksum
        );
    }
}
