//! The μTZ lookup bench on ESP32-S3: the on-target flash-latency matrix.
//!
//! The firmware embeds each preset shape (tiny / compact / balanced) twice
//! (the preset's compressed asset and its uncompressed twin) and measures
//! every memory mode the hardware supports:
//!
//! - **xip-flash** calls `Finder::from_static()` on the uncompressed blob;
//!   lookups stream straight out of memory-mapped flash, and the payload is
//!   never in RAM.
//! - **ram** copies the uncompressed container into the heap (`from_vec()`)
//!   and streams PIP from RAM. Small payloads land in internal SRAM; a
//!   sacrificial SRAM filler forces a second tiny run into PSRAM, isolating
//!   the PSRAM access penalty.
//! - **decode** calls `from_slice()` on the compressed asset, the
//!   buffered-decode path; the decode time is printed separately and gives
//!   the per-codec embedded decode speed.
//! - **eager** runs `from_static()` plus `preload()`; geometry is decoded to
//!   RAM once, and the payload stays in flash.
//! - **partition** reads the same tiny-static asset back out of a dedicated
//!   `utzdata` flash partition (found by label in the ESP-IDF partition
//!   table at runtime) instead of the app image; this is the
//!   ship-the-dataset-separately path, e.g. for OTA-ing data without the
//!   firmware.
//!
//! The bench uses the same harness and points as `utz_bench_cli`: every
//! leg's checksum must equal the host run of the same shape at npts=2000.
//!
//! One-time setup is described in README.md. After that,
//! `cargo run --release` flashes and monitors.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use esp_alloc::MemoryCapability;
use esp_backtrace as _;
use esp_hal::main;
use esp_hal::time::Instant;
use esp_println::println;
use utz::Finder;

// app descriptor required by espflash ≥4 image validation
esp_bootloader_esp_idf::esp_app_desc!();

// preset assets from the utz_data_* crates (utz preset features; regenerate
// with scripts/gen-presets.sh)…
use utz::data::{
    BALANCED as BALANCED_BR, COMPACT as COMPACT_XZ, TINY as TINY_GZ, TINY_STATIC as TINY_NONE,
};
// …and the shared custom shapes: uncompressed twins (from_static accepts
// only codec none) across all geometry encodings (recipes + capability
// guards in utz_bench_common's build.rs)
use utz_bench_common::assets::{
    BALANCED_NONE, COMPACT_EAGER, COMPACT_FIXED, COMPACT_NONE, TINY_COARSE, TINY_EAGER, TINY_FIXED,
};

/// A modest point count by host standards; lookups run ~250-300x host on
/// this core (see README), so a round must stay in seconds, not minutes.
const NPTS: usize = 2_000;
const ROUNDS: usize = 3;
/// The internal-SRAM heap size; the PSRAM region is added at runtime if
/// detected. It is not larger because the rest of DRAM is the main stack,
/// and gzip decode keeps its ~32 K inflate state there (a 320 K heap
/// tripped the stack guard).
const SRAM_HEAP: usize = 256 * 1024;

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

fn free_sram() -> usize {
    esp_alloc::HEAP.free_caps(MemoryCapability::Internal.into())
}

fn free_psram() -> usize {
    esp_alloc::HEAP.free_caps(MemoryCapability::External.into())
}

/// Reports whether a single allocation of `bytes` can plausibly be
/// satisfied (regions don't combine).
fn fits(bytes: usize) -> bool {
    free_sram().max(free_psram()) > bytes + 32 * 1024
}

fn region_of(address: usize, psram: &core::ops::Range<usize>) -> &'static str {
    if psram.contains(&address) {
        "PSRAM"
    } else if (0x3FC8_0000..0x3FD0_0000).contains(&address) {
        "SRAM"
    } else {
        "flash"
    }
}

fn bench(label: &str, finder: &Finder, points: &[(f64, f64)]) {
    let mut time_source = now_us;
    let result = utz_bench_common::run_rounds(finder, points, ROUNDS, &mut time_source);
    println!(
        "RESULT {}: {} lookups · {} hits · {} us · {} us/lookup · checksum {}",
        label,
        result.lookups,
        result.hits,
        result.elapsed_us,
        result.elapsed_us / result.lookups as u64,
        result.checksum
    );
}

/// Runs the xip-flash leg: the payload is borrowed from memory-mapped
/// flash, zero-copy.
fn xip_leg(label: &str, blob: &'static [u8], points: &[(f64, f64)]) {
    let finder = Finder::from_static(blob).expect("from_static");
    bench(label, &finder, points);
}

/// Runs the ram leg: the uncompressed container is copied to the heap, and
/// PIP streams from RAM.
fn ram_leg(
    label: &str,
    blob: &'static [u8],
    points: &[(f64, f64)],
    psram: &core::ops::Range<usize>,
) {
    if !fits(blob.len()) {
        println!("SKIP {}: {} KiB payload does not fit any heap region", label, blob.len() / 1024);
        return;
    }
    let bytes = blob.to_vec();
    // from_vec reuses this allocation (copy_within + truncate), so the
    // pointer taken here is where the payload actually lives during lookups
    let payload_region = region_of(bytes.as_ptr() as usize, psram);
    let finder = Finder::from_vec(bytes).expect("from_vec");
    println!("INFO {}: payload in {}", label, payload_region);
    bench(label, &finder, points);
}

/// Runs the decode leg: the compressed asset is read from flash, and the
/// payload is decoded into the heap.
fn decode_leg(
    label: &str,
    blob: &'static [u8],
    decoded_hint: usize,
    points: &[(f64, f64)],
) {
    if !fits(decoded_hint) {
        println!("SKIP {}: ~{} KiB decoded payload does not fit any heap region", label, decoded_hint / 1024);
        return;
    }
    let (sram_before, psram_before) = (free_sram() as isize, free_psram() as isize);
    let start_us = now_us();
    let finder = Finder::from_slice(blob).expect("from_slice");
    let decode_us = now_us() - start_us;
    let (sram_after, psram_after) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: decode {} ms ({} KiB compressed), heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        decode_us / 1000,
        blob.len() / 1024,
        (sram_before - sram_after) / 1024,
        (psram_before - psram_after) / 1024
    );
    bench(label, &finder, points);
}

/// Runs the eager leg: the payload stays in flash, and all geometry is
/// decoded to the heap once.
fn eager_leg(label: &str, blob: &'static [u8], points: &[(f64, f64)]) {
    let mut finder = Finder::from_static(blob).expect("from_static");
    // exact requirement from the header's eager counts; preload reserves
    // exactly (no growth doubling), so fit means fit
    let needed_bytes = finder.preload_size();
    if !fits(needed_bytes) {
        println!("SKIP {}: eager cache needs {} KiB — no heap region fits", label, needed_bytes / 1024);
        return;
    }
    let (sram_before, psram_before) = (free_sram() as isize, free_psram() as isize);
    let start_us = now_us();
    finder.preload();
    let preload_us = now_us() - start_us;
    let (sram_after, psram_after) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: preload {} ms, heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        preload_us / 1000,
        (sram_before - sram_after) / 1024,
        (psram_before - psram_after) / 1024
    );
    bench(label, &finder, points);
}

/// Runs the partition leg, where the dataset is NOT in the app image. It
/// sits in its own `utzdata` flash partition (partitions.csv), written by
/// flash-with-data.sh. At runtime the leg parses the ESP-IDF partition
/// table, finds the partition by label, sizes the read from the container's
/// outer header (the partition is bigger than the asset; erased flash is
/// 0xFF), copies to the heap, and benches. The asset is the tiny-static
/// preset, so the bytes (and the checksum) must match the embedded
/// TINY_NONE legs.
fn partition_leg(
    label: &str,
    flash: esp_hal::peripherals::FLASH<'static>,
    twin: &'static [u8],
    points: &[(f64, f64)],
    psram: &core::ops::Range<usize>,
) {
    use embedded_storage::ReadStorage;
    use esp_bootloader_esp_idf::partitions;

    let mut flash = esp_storage::FlashStorage::new(flash);
    let mut table_buffer = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let table = match partitions::read_partition_table(&mut flash, &mut table_buffer) {
        Ok(table) => table,
        Err(error) => {
            println!("SKIP {}: partition table unreadable ({:?})", label, error);
            return;
        }
    };
    let Some(partition) = table.iter().find(|entry| entry.label_as_str() == "utzdata") else {
        println!("SKIP {}: no utzdata partition — flash via flash-with-data.sh", label);
        return;
    };
    println!(
        "INFO {}: utzdata partition at {:#x}, {} KiB",
        label,
        partition.offset(),
        partition.len() / 1024
    );
    let mut region = partition.as_embedded_storage(&mut flash);
    let mut header = [0u8; utz::format::OUTER_LEN];
    if region.read(0, &mut header).is_err() {
        println!("SKIP {}: header read failed", label);
        return;
    }
    let total = match utz::format::outer(&header) {
        // codec none: the container is exactly header + raw payload
        Ok((0, raw_len, start)) => start + raw_len,
        _ => {
            println!("SKIP {}: partition holds no uncompressed container — reflash it", label);
            return;
        }
    };
    if total > region.capacity() || !fits(total) {
        println!("SKIP {}: {} KiB container too big for partition or heap", label, total / 1024);
        return;
    }
    let mut bytes = alloc::vec![0u8; total];
    let payload_region = region_of(bytes.as_ptr() as usize, psram);
    let start_us = now_us();
    // bounce through a stack chunk: the ROM read runs with the cache
    // disabled, so its destination must be internal RAM — the heap buffer
    // may be PSRAM, which is only reachable through the cache
    let mut chunk = [0u8; 4096];
    let mut offset = 0usize;
    while offset < total {
        let chunk_len = (total - offset).min(chunk.len());
        if region.read(offset as u32, &mut chunk[..chunk_len]).is_err() {
            println!("SKIP {}: flash read failed at offset {:#x}", label, offset);
            return;
        }
        bytes[offset..offset + chunk_len].copy_from_slice(&chunk[..chunk_len]);
        offset += chunk_len;
    }
    let read_us = now_us() - start_us;
    println!(
        "INFO {}: read {} KiB to {} in {} ms — {} the embedded twin",
        label,
        total / 1024,
        payload_region,
        read_us / 1000,
        if bytes == twin { "matches" } else { "DIFFERS FROM" }
    );
    let finder = Finder::from_vec(bytes).expect("from_vec");
    bench(label, &finder, points);
}

/// Runs the eager_from_slice leg: the compressed asset is decoded straight
/// to eager and the geometry sections are dropped, so the steady-state heap
/// is the eager cache plus header/tzid/grid only (compare against
/// decode + preload's payload+cache).
fn eager_slice_leg(label: &str, blob: &'static [u8], points: &[(f64, f64)]) {
    let (sram_before, psram_before) = (free_sram() as isize, free_psram() as isize);
    let start_us = now_us();
    let finder = match Finder::eager_from_slice(blob) {
        Ok(finder) => finder,
        Err(_) => {
            println!("SKIP {}: eager_from_slice failed (no heap fit?)", label);
            return;
        }
    };
    let load_us = now_us() - start_us;
    let (sram_after, psram_after) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: load {} ms ({} KiB compressed), steady heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        load_us / 1000,
        blob.len() / 1024,
        (sram_before - sram_after) / 1024,
        (psram_before - psram_after) / 1024
    );
    bench(label, &finder, points);
}

#[main]
fn main() -> ! {
    // Config::default() would boot at 80 MHz — bench at the chip's 240 MHz
    let peripherals = esp_hal::init(
        esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::max()),
    );
    esp_alloc::heap_allocator!(size: SRAM_HEAP);

    // N16R8 module: 8 MB octal PSRAM. Auto mode probes octal then quad; on a
    // PSRAM-less module this prints 0 KiB and the big RAM legs SKIP.
    let psram_device = esp_hal::psram::Psram::new(
        peripherals.PSRAM,
        esp_hal::psram::PsramConfig::default(),
    );
    let (psram_ptr, psram_len) = psram_device.raw_parts();
    if psram_len > 0 {
        unsafe {
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                psram_ptr,
                psram_len,
                MemoryCapability::External.into(),
            ));
        }
    }
    let psram = psram_ptr as usize..psram_ptr as usize + psram_len;

    println!(
        "uTZ bench on ESP32-S3 @ 240 MHz — SRAM heap {} KiB, PSRAM {} KiB",
        SRAM_HEAP / 1024,
        psram_len / 1024
    );
    println!(
        "tzbb release: {:?} — {} pts, {} rounds, fastest round wins",
        Finder::from_static(TINY_NONE).expect("container decode").tzbb_release(),
        NPTS,
        ROUNDS
    );

    // allocated first so the points sit in SRAM for every leg
    let points = utz_bench_common::gen_pts(NPTS);

    // --- streaming from flash (XIP, zero-copy) ---
    xip_leg("tiny xip-flash", TINY_NONE, &points);
    // fixed-width arcs: same geometry, no per-vertex varint decode;
    // tiny = i16, compact = i24 (heavier read_fixed byte assembly)
    xip_leg("tiny-fixed xip-flash", TINY_FIXED, &points);
    xip_leg("compact xip-flash", COMPACT_NONE, &points);
    xip_leg("compact-fixed xip-flash", COMPACT_FIXED, &points);
    // eager-image: slice kernels straight off flash — eager speed, zero RAM
    xip_leg("tiny-eager xip-flash", TINY_EAGER, &points);
    xip_leg("compact-eager xip-flash", COMPACT_EAGER, &points);
    // grid-only: lookup() == lookup_coarse (cell precision; own checksum)
    xip_leg("tiny-coarse xip-flash", TINY_COARSE, &points);
    xip_leg("balanced xip-flash", BALANCED_NONE, &points);

    // --- streaming from RAM (uncompressed copy) ---
    ram_leg("tiny ram", TINY_NONE, &points, &psram); // fits SRAM
    if psram_len > 0 {
        // fill SRAM so the same payload is forced into PSRAM: the direct
        // SRAM-vs-PSRAM lookup comparison
        let mut filler: Vec<Vec<u8>> = Vec::new();
        while free_sram() > 24 * 1024 {
            filler.push(alloc::vec![0u8; 16 * 1024]);
        }
        ram_leg("tiny ram-psram", TINY_NONE, &points, &psram);
        drop(filler);
    } else {
        println!("SKIP tiny ram-psram: no PSRAM");
    }
    ram_leg("compact ram", COMPACT_NONE, &points, &psram);
    ram_leg("balanced ram", BALANCED_NONE, &points, &psram);

    // --- flash partition (dataset outside the app image, found at runtime) ---
    partition_leg("tiny partition", peripherals.FLASH, TINY_NONE, &points, &psram);

    // --- buffered decode (compressed asset in flash → payload in RAM) ---
    decode_leg("tiny decode-gzip", TINY_GZ, TINY_NONE.len(), &points);
    decode_leg("compact decode-xz", COMPACT_XZ, COMPACT_NONE.len(), &points);
    decode_leg("balanced decode-brotli", BALANCED_BR, BALANCED_NONE.len(), &points);

    // --- eager (payload in flash, geometry cache in RAM) ---
    eager_leg("tiny eager", TINY_NONE, &points);
    eager_leg("compact eager", COMPACT_NONE, &points);
    eager_leg("balanced eager", BALANCED_NONE, &points);

    // --- eager_from_slice (compressed asset → eager, geometry dropped) ---
    eager_slice_leg("tiny eager-slice", TINY_GZ, &points);

    kernel_bench();
    kernel_bench_i16();
    kernel_bench_i32();
    kernel_bench_15_bit_quant();

    println!("DONE");
    loop {}
}

/// Compares the PIP kernels with no container involved: one synthetic
/// i24-range ring is folded through each arithmetic width on the identical
/// slice. Random vertices are fine: even-odd parity is well-defined on any
/// closed polyline and all three kernels implement the same rule, so
/// results must agree exactly (f64 is bit-exact at i24; see the pip.rs
/// module docs). The branch mix differs from real geometry (~50% y-span
/// hits), so read it as a kernel ratio, not an absolute lookup cost.
fn kernel_bench() {
    use utz::pip::{ring_hit, RingHit};
    const RING_LEN: usize = 8192;
    const PROBES: usize = 200;
    const COORD_RANGE: i64 = 1 << 23;
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (((lcg >> 33) as i64 % COORD_RANGE) - COORD_RANGE / 2) as i32
    };
    let ring: Vec<(i32, i32)> = (0..RING_LEN).map(|_| (next(), next())).collect();
    let probes: Vec<(i32, i32)> = (0..PROBES).map(|_| (next(), next())).collect();

    let code = |hit: RingHit| -> u64 {
        match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run = |kernel: fn(&[(i32, i32)], i32, i32) -> RingHit| -> (u64, u64) {
        let start_us = now_us();
        let mut fingerprint = 0u64; // result fingerprint; also defeats elision
        for &(probe_x, probe_y) in &probes {
            fingerprint = fingerprint
                .wrapping_mul(3)
                .wrapping_add(code(kernel(&ring, probe_x, probe_y)));
        }
        (now_us() - start_us, fingerprint)
    };
    let (i64_us, i64_fingerprint) = run(ring_hit::<i64, (i32, i32)>);
    let (i128_us, i128_fingerprint) = run(ring_hit::<i128, (i32, i32)>);
    let (f64_us, f64_fingerprint) = run(ring_hit::<f64, (i32, i32)>);
    let (split_us, split_fingerprint) = run(utz::pip::ring_hit_split::<(i32, i32)>);
    assert!(
        i64_fingerprint == i128_fingerprint
            && i64_fingerprint == f64_fingerprint
            && i64_fingerprint == split_fingerprint,
        "kernel results disagree"
    );
    let edges = (RING_LEN * PROBES) as u64;
    println!(
        "KERNEL {} edges: i64 {} us ({} ns/edge) · i128 {} us ({:.2}x) · f64 {} us ({:.2}x) · split-u64 {} us ({:.2}x) · results agree",
        edges,
        i64_us,
        i64_us * 1000 / edges,
        i128_us,
        i128_us as f64 / i64_us as f64,
        f64_us,
        f64_us as f64 / i64_us as f64,
        split_us,
        split_us as f64 / i64_us as f64
    );
}

/// Runs the i32-quant kernel matrix: the sign-split u64 kernel races the
/// i128 kernel over a FULL-i32-range ring. They are the only two exact
/// kernels at this width (i64 overflows, f64 is inexact), so the pair must
/// agree, and their ratio is the "retire i128 on 32-bit cores" answer.
fn kernel_bench_i32() {
    use utz::pip::{ring_hit, ring_hit_split, RingHit};
    const RING_LEN: usize = 8192;
    const PROBES: usize = 200;
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((lcg >> 32) as u32) as i32 // full i32 range
    };
    let ring: Vec<(i32, i32)> = (0..RING_LEN).map(|_| (next(), next())).collect();
    let probes: Vec<(i32, i32)> = (0..PROBES).map(|_| (next(), next())).collect();

    let code = |hit: RingHit| -> u64 {
        match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run = |kernel: fn(&[(i32, i32)], i32, i32) -> RingHit| -> (u64, u64) {
        let start_us = now_us();
        let mut fingerprint = 0u64;
        for &(probe_x, probe_y) in &probes {
            fingerprint = fingerprint
                .wrapping_mul(3)
                .wrapping_add(code(kernel(&ring, probe_x, probe_y)));
        }
        (now_us() - start_us, fingerprint)
    };
    let (i128_us, i128_fingerprint) = run(ring_hit::<i128, (i32, i32)>);
    let (split_us, split_fingerprint) = run(ring_hit_split::<(i32, i32)>);
    assert!(i128_fingerprint == split_fingerprint, "i32 kernel results disagree");
    let edges = (RING_LEN * PROBES) as u64;
    println!(
        "KERNEL32 {} edges: i128 {} us ({} ns/edge) · split-u64 {} us ({:.2}x) · results agree",
        edges,
        i128_us,
        i128_us * 1000 / edges,
        split_us,
        split_us as f64 / i128_us as f64
    );
}

/// Runs the i16 kernel matrix: the shipped sign-split kernel
/// (`pip::ring_hit_split()`, what i16-quant eager/image lookups dispatch)
/// races the generic i64 kernel on the identical `(i16, i16)` slice, plus
/// the same geometry widened to `(i32, i32)` pairs for the load-width
/// effect. The ring spans the full i16 range so worst-case products are
/// exercised (65535² just fits u32; see `pip::edge_split()`); ring-level
/// results must agree exactly (the sign-split kernel may flag Boundary via
/// a different edge of the same vertex, but Boundary short-circuits the
/// ring either way).
fn kernel_bench_i16() {
    use utz::pip::{ring_hit, ring_hit_split, RingHit};
    const RING_LEN: usize = 8192;
    const PROBES: usize = 200;
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((lcg >> 48) as u16) as i16 // full i16 range
    };
    let ring16: Vec<(i16, i16)> = (0..RING_LEN).map(|_| (next(), next())).collect();
    let probes16: Vec<(i16, i16)> = (0..PROBES).map(|_| (next(), next())).collect();
    let ring32: Vec<(i32, i32)> = ring16.iter().map(|&(x, y)| (i32::from(x), i32::from(y))).collect();

    let code = |hit: RingHit| -> u64 {
        match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run16 = |kernel: fn(&[(i16, i16)], i16, i16) -> RingHit| -> (u64, u64) {
        let start_us = now_us();
        let mut fingerprint = 0u64;
        for &(probe_x, probe_y) in &probes16 {
            fingerprint = fingerprint
                .wrapping_mul(3)
                .wrapping_add(code(kernel(&ring16, probe_x, probe_y)));
        }
        (now_us() - start_us, fingerprint)
    };
    let run32 = |kernel: fn(&[(i32, i32)], i32, i32) -> RingHit| -> (u64, u64) {
        let start_us = now_us();
        let mut fingerprint = 0u64;
        for &(probe_x, probe_y) in &probes16 {
            fingerprint = fingerprint.wrapping_mul(3).wrapping_add(code(kernel(
                &ring32,
                i32::from(probe_x),
                i32::from(probe_y),
            )));
        }
        (now_us() - start_us, fingerprint)
    };
    let (wide_i32_us, wide_i32_fingerprint) = run32(ring_hit::<i64, (i32, i32)>);
    let (narrow_i64_us, narrow_i64_fingerprint) = run16(ring_hit::<i64, (i16, i16)>);
    let (split_u32_us, split_u32_fingerprint) = run16(ring_hit_split::<(i16, i16)>);
    assert!(
        narrow_i64_fingerprint == wide_i32_fingerprint
            && narrow_i64_fingerprint == split_u32_fingerprint,
        "i16 kernel results disagree"
    );
    let edges = (RING_LEN * PROBES) as u64;
    println!(
        "KERNEL16 {} edges: i64/i16-pairs {} us ({} ns/edge) · u32-signsplit/i16-pairs {} us ({:.2}x) · i64/i32-pairs {} us ({:.2}x) · results agree",
        edges,
        narrow_i64_us,
        narrow_i64_us * 1000 / edges,
        split_u32_us,
        split_u32_us as f64 / narrow_i64_us as f64,
        wide_i32_us,
        wide_i32_us as f64 / narrow_i64_us as f64
    );
}

/// Answers the 15-bit-quant question: quantizing one bit shy of the
/// storage width (|coord| ≤ 2^14) makes the plain compare-form kernel exact
/// at `W = i32` (differences fit 15 bits, and each cross-product half fits
/// 2^30) with no swap and no sign classification. The bench races it
/// against the i64 kernel and the sign-split kernel on the identical
/// 15-bit-range `(i16, i16)` slice; all three are exact here, so results
/// must agree.
fn kernel_bench_15_bit_quant() {
    use utz::pip::{ring_hit, ring_hit_split, RingHit};
    const RING_LEN: usize = 8192;
    const PROBES: usize = 200;
    const COORD_RANGE: i64 = 1 << 15; // draws in [−2^14, 2^14−1]
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (((lcg >> 33) as i64 % COORD_RANGE) - COORD_RANGE / 2) as i16
    };
    let ring: Vec<(i16, i16)> = (0..RING_LEN).map(|_| (next(), next())).collect();
    let probes: Vec<(i16, i16)> = (0..PROBES).map(|_| (next(), next())).collect();

    let code = |hit: RingHit| -> u64 {
        match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run = |kernel: fn(&[(i16, i16)], i16, i16) -> RingHit| -> (u64, u64) {
        let start_us = now_us();
        let mut fingerprint = 0u64;
        for &(probe_x, probe_y) in &probes {
            fingerprint = fingerprint
                .wrapping_mul(3)
                .wrapping_add(code(kernel(&ring, probe_x, probe_y)));
        }
        (now_us() - start_us, fingerprint)
    };
    let (i64_us, i64_fingerprint) = run(ring_hit::<i64, (i16, i16)>);
    let (i32_us, i32_fingerprint) = run(ring_hit::<i32, (i16, i16)>);
    let (split_us, split_fingerprint) = run(ring_hit_split::<(i16, i16)>);
    assert!(
        i64_fingerprint == i32_fingerprint && i64_fingerprint == split_fingerprint,
        "15-bit kernel results disagree"
    );
    let edges = (RING_LEN * PROBES) as u64;
    println!(
        "KERNEL15 {} edges: i64 {} us ({} ns/edge) · i32 {} us ({:.2}x) · split-u32 {} us ({:.2}x) · results agree",
        edges,
        i64_us,
        i64_us * 1000 / edges,
        i32_us,
        i32_us as f64 / i64_us as f64,
        split_us,
        split_us as f64 / i64_us as f64
    );
}
