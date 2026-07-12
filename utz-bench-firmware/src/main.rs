//! μTZ lookup bench on ESP32-S3 — the PLAN §15 flash-latency matrix.
//!
//! Embeds each preset shape (tiny / compact / balanced) twice — the preset's
//! compressed asset and its uncompressed twin — and measures every memory
//! mode the hardware supports:
//!
//! - **xip-flash**: `Finder::from_static` on the uncompressed blob — lookups
//!   stream straight out of memory-mapped flash, payload never in RAM.
//! - **ram**: the uncompressed container copied into heap (`from_vec`) —
//!   streaming PIP from RAM. Small payloads land in internal SRAM; a
//!   sacrificial SRAM filler forces a second tiny run into PSRAM, isolating
//!   the PSRAM access penalty.
//! - **decode**: `from_slice` on the compressed asset — the buffered-decode
//!   path (decode time printed separately = per-codec embedded decode speed).
//! - **eager**: `from_static` + `preload` — geometry decoded to RAM once,
//!   payload stays in flash.
//! - **partition**: the same tiny-static asset read back out of a dedicated
//!   `utzdata` flash partition (found by label in the ESP-IDF partition
//!   table at runtime) instead of the app image — the ship-the-dataset-
//!   separately path, e.g. for OTA-ing data without the firmware.
//!
//! Uses the same harness + points as utz-bench-cli: every leg's checksum must
//! equal the host run of the same shape at npts=2000.
//!
//! Setup (once): see README.md. Then `cargo run --release` flashes + monitors.

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

// preset assets from the utz-data-* crates (utz preset features; regenerate
// with scripts/gen-presets.sh)…
use utz::data::{
    BALANCED as BALANCED_BR, COMPACT as COMPACT_XZ, TINY as TINY_GZ, TINY_STATIC as TINY_NONE,
};
// …and the shared custom shapes: uncompressed twins (from_static accepts
// only codec none) across all geometry encodings (recipes + capability
// guards in utz-bench-common's build.rs)
use utz_bench_common::assets::{
    BALANCED_NONE, COMPACT_EAGER, COMPACT_FIXED, COMPACT_NONE, TINY_COARSE, TINY_EAGER, TINY_FIXED,
};

/// modest by host standards; lookups run ~250-300x host on this core (see
/// README) so a round must stay in seconds, not minutes
const NPTS: usize = 2_000;
const ROUNDS: usize = 3;
/// internal-SRAM heap; the PSRAM region is added at runtime if detected.
/// Not larger: the rest of DRAM is the main stack, and gzip decode keeps its
/// ~32 K inflate state there (320 K heap tripped the stack guard).
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

/// largest single allocation we can plausibly satisfy (regions don't combine)
fn fits(bytes: usize) -> bool {
    free_sram().max(free_psram()) > bytes + 32 * 1024
}

fn region_of(addr: usize, psram: &core::ops::Range<usize>) -> &'static str {
    if psram.contains(&addr) {
        "PSRAM"
    } else if (0x3FC8_0000..0x3FD0_0000).contains(&addr) {
        "SRAM"
    } else {
        "flash"
    }
}

fn bench(label: &str, finder: &Finder, pts: &[(f64, f64)]) {
    let mut now = now_us;
    let r = utz_bench_common::run_rounds(finder, pts, ROUNDS, &mut now);
    println!(
        "RESULT {}: {} lookups · {} hits · {} us · {} us/lookup · checksum {}",
        label,
        r.lookups,
        r.hits,
        r.elapsed_us,
        r.elapsed_us / r.lookups as u64,
        r.checksum
    );
}

/// xip-flash leg: payload borrowed from memory-mapped flash, zero-copy
fn xip_leg(label: &str, blob: &'static [u8], pts: &[(f64, f64)]) {
    let f = Finder::from_static(blob).expect("from_static");
    bench(label, &f, pts);
}

/// ram leg: uncompressed container copied to heap, PIP streams from RAM
fn ram_leg(label: &str, blob: &'static [u8], pts: &[(f64, f64)], psram: &core::ops::Range<usize>) {
    if !fits(blob.len()) {
        println!("SKIP {}: {} KiB payload does not fit any heap region", label, blob.len() / 1024);
        return;
    }
    let v = blob.to_vec();
    // from_vec reuses this allocation (copy_within + truncate), so the
    // pointer taken here is where the payload actually lives during lookups
    let where_ = region_of(v.as_ptr() as usize, psram);
    let f = Finder::from_vec(v).expect("from_vec");
    println!("INFO {}: payload in {}", label, where_);
    bench(label, &f, pts);
}

/// decode leg: compressed asset read from flash, payload decoded into heap
fn decode_leg(
    label: &str,
    blob: &'static [u8],
    decoded_hint: usize,
    pts: &[(f64, f64)],
) {
    if !fits(decoded_hint) {
        println!("SKIP {}: ~{} KiB decoded payload does not fit any heap region", label, decoded_hint / 1024);
        return;
    }
    let (s0, p0) = (free_sram() as isize, free_psram() as isize);
    let t0 = now_us();
    let f = Finder::from_slice(blob).expect("from_slice");
    let decode_us = now_us() - t0;
    let (s1, p1) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: decode {} ms ({} KiB compressed), heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        decode_us / 1000,
        blob.len() / 1024,
        (s0 - s1) / 1024,
        (p0 - p1) / 1024
    );
    bench(label, &f, pts);
}

/// eager leg: payload stays in flash, all geometry decoded to heap once
fn eager_leg(label: &str, blob: &'static [u8], pts: &[(f64, f64)]) {
    let mut f = Finder::from_static(blob).expect("from_static");
    // exact requirement from the v2 header counts; preload reserves exactly
    // (no growth doubling), so fit means fit
    let need = f.preload_bytes();
    if !fits(need) {
        println!("SKIP {}: eager cache needs {} KiB — no heap region fits", label, need / 1024);
        return;
    }
    let (s0, p0) = (free_sram() as isize, free_psram() as isize);
    let t0 = now_us();
    f.preload();
    let preload_us = now_us() - t0;
    let (s1, p1) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: preload {} ms, heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        preload_us / 1000,
        (s0 - s1) / 1024,
        (p0 - p1) / 1024
    );
    bench(label, &f, pts);
}

/// partition leg: the dataset is NOT in the app image — it sits in its own
/// `utzdata` flash partition (partitions.csv), written by flash-with-data.sh.
/// At runtime: parse the ESP-IDF partition table, find the partition by
/// label, size the read from the container's outer header (the partition is
/// bigger than the asset; erased flash is 0xFF), copy to heap, bench. The
/// asset is the tiny-static preset, so the bytes — and the checksum — must
/// match the embedded TINY_NONE legs.
fn partition_leg(
    label: &str,
    flash: esp_hal::peripherals::FLASH<'static>,
    twin: &'static [u8],
    pts: &[(f64, f64)],
    psram: &core::ops::Range<usize>,
) {
    use embedded_storage::ReadStorage;
    use esp_bootloader_esp_idf::partitions;

    let mut flash = esp_storage::FlashStorage::new(flash);
    let mut pt_mem = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
    let table = match partitions::read_partition_table(&mut flash, &mut pt_mem) {
        Ok(t) => t,
        Err(e) => {
            println!("SKIP {}: partition table unreadable ({:?})", label, e);
            return;
        }
    };
    let Some(part) = table.iter().find(|p| p.label_as_str() == "utzdata") else {
        println!("SKIP {}: no utzdata partition — flash via flash-with-data.sh", label);
        return;
    };
    println!(
        "INFO {}: utzdata partition at {:#x}, {} KiB",
        label,
        part.offset(),
        part.len() / 1024
    );
    let mut region = part.as_embedded_storage(&mut flash);
    let mut head = [0u8; utz::format::OUTER_LEN];
    if region.read(0, &mut head).is_err() {
        println!("SKIP {}: header read failed", label);
        return;
    }
    let total = match utz::format::outer(&head) {
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
    let mut v = alloc::vec![0u8; total];
    let where_ = region_of(v.as_ptr() as usize, psram);
    let t0 = now_us();
    // bounce through a stack chunk: the ROM read runs with the cache
    // disabled, so its destination must be internal RAM — the heap buffer
    // may be PSRAM, which is only reachable through the cache
    let mut chunk = [0u8; 4096];
    let mut off = 0usize;
    while off < total {
        let n = (total - off).min(chunk.len());
        if region.read(off as u32, &mut chunk[..n]).is_err() {
            println!("SKIP {}: flash read failed at offset {:#x}", label, off);
            return;
        }
        v[off..off + n].copy_from_slice(&chunk[..n]);
        off += n;
    }
    let read_us = now_us() - t0;
    println!(
        "INFO {}: read {} KiB to {} in {} ms — {} the embedded twin",
        label,
        total / 1024,
        where_,
        read_us / 1000,
        if v == twin { "matches" } else { "DIFFERS FROM" }
    );
    let f = Finder::from_vec(v).expect("from_vec");
    bench(label, &f, pts);
}

/// eager_from_slice leg: compressed asset decoded straight to eager, the
/// geometry sections dropped — steady-state heap is the eager cache plus
/// header/tzid/grid only (compare against decode + preload's payload+cache)
fn eager_slice_leg(label: &str, blob: &'static [u8], pts: &[(f64, f64)]) {
    let (s0, p0) = (free_sram() as isize, free_psram() as isize);
    let t0 = now_us();
    let f = match Finder::eager_from_slice(blob) {
        Ok(f) => f,
        Err(_) => {
            println!("SKIP {}: eager_from_slice failed (no heap fit?)", label);
            return;
        }
    };
    let load_us = now_us() - t0;
    let (s1, p1) = (free_sram() as isize, free_psram() as isize);
    println!(
        "INFO {}: load {} ms ({} KiB compressed), steady heap dSRAM {} KiB dPSRAM {} KiB",
        label,
        load_us / 1000,
        blob.len() / 1024,
        (s0 - s1) / 1024,
        (p0 - p1) / 1024
    );
    bench(label, &f, pts);
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
    let psram_dev = esp_hal::psram::Psram::new(
        peripherals.PSRAM,
        esp_hal::psram::PsramConfig::default(),
    );
    let (ps_ptr, ps_len) = psram_dev.raw_parts();
    if ps_len > 0 {
        unsafe {
            esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
                ps_ptr,
                ps_len,
                MemoryCapability::External.into(),
            ));
        }
    }
    let psram = ps_ptr as usize..ps_ptr as usize + ps_len;

    println!(
        "uTZ bench on ESP32-S3 @ 240 MHz — SRAM heap {} KiB, PSRAM {} KiB",
        SRAM_HEAP / 1024,
        ps_len / 1024
    );
    println!(
        "tzbb release: {:?} — {} pts, {} rounds, fastest round wins",
        Finder::from_static(TINY_NONE).expect("container decode").tzbb_release(),
        NPTS,
        ROUNDS
    );

    // allocated first so the points sit in SRAM for every leg
    let pts = utz_bench_common::gen_pts(NPTS);

    // --- streaming from flash (XIP, zero-copy) ---
    xip_leg("tiny xip-flash", TINY_NONE, &pts);
    // fixed-width arcs: same geometry, no per-vertex varint decode (§13);
    // tiny = i16, compact = i24 (heavier read_fixed byte assembly)
    xip_leg("tiny-fixed xip-flash", TINY_FIXED, &pts);
    xip_leg("compact xip-flash", COMPACT_NONE, &pts);
    xip_leg("compact-fixed xip-flash", COMPACT_FIXED, &pts);
    // eager-image: slice kernels straight off flash — eager speed, zero RAM
    xip_leg("tiny-eager xip-flash", TINY_EAGER, &pts);
    xip_leg("compact-eager xip-flash", COMPACT_EAGER, &pts);
    // grid-only: lookup() == lookup_coarse (cell precision; own checksum)
    xip_leg("tiny-coarse xip-flash", TINY_COARSE, &pts);
    xip_leg("balanced xip-flash", BALANCED_NONE, &pts);

    // --- streaming from RAM (uncompressed copy) ---
    ram_leg("tiny ram", TINY_NONE, &pts, &psram); // fits SRAM
    if ps_len > 0 {
        // fill SRAM so the same payload is forced into PSRAM: the direct
        // SRAM-vs-PSRAM lookup comparison
        let mut filler: Vec<Vec<u8>> = Vec::new();
        while free_sram() > 24 * 1024 {
            filler.push(alloc::vec![0u8; 16 * 1024]);
        }
        ram_leg("tiny ram-psram", TINY_NONE, &pts, &psram);
        drop(filler);
    } else {
        println!("SKIP tiny ram-psram: no PSRAM");
    }
    ram_leg("compact ram", COMPACT_NONE, &pts, &psram);
    ram_leg("balanced ram", BALANCED_NONE, &pts, &psram);

    // --- flash partition (dataset outside the app image, found at runtime) ---
    partition_leg("tiny partition", peripherals.FLASH, TINY_NONE, &pts, &psram);

    // --- buffered decode (compressed asset in flash → payload in RAM) ---
    decode_leg("tiny decode-gzip", TINY_GZ, TINY_NONE.len(), &pts);
    decode_leg("compact decode-xz", COMPACT_XZ, COMPACT_NONE.len(), &pts);
    decode_leg("balanced decode-brotli", BALANCED_BR, BALANCED_NONE.len(), &pts);

    // --- eager (payload in flash, geometry cache in RAM) ---
    eager_leg("tiny eager", TINY_NONE, &pts);
    eager_leg("compact eager", COMPACT_NONE, &pts);
    eager_leg("balanced eager", BALANCED_NONE, &pts);

    // --- eager_from_slice (compressed asset → eager, geometry dropped) ---
    eager_slice_leg("tiny eager-slice", TINY_GZ, &pts);

    kernel_bench();
    kernel_bench_i16();

    println!("DONE");
    loop {}
}

/// PIP kernel comparison, no container involved: one synthetic i24-range
/// ring folded through each arithmetic width on the identical slice.
/// Random vertices are fine — even-odd parity is well-defined on any closed
/// polyline and all three kernels implement the same rule, so verdicts must
/// agree exactly (f64 is bit-exact at i24 — pip.rs module docs). Branch mix
/// differs from real geometry (~50% y-span hits), so read it as a kernel
/// ratio, not an absolute lookup cost.
fn kernel_bench() {
    use utz::pip::{ring_hit, RingHit};
    const N: usize = 8192;
    const PROBES: usize = 200;
    const M: i64 = 1 << 23;
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (((lcg >> 33) as i64 % M) - M / 2) as i32
    };
    let ring: Vec<(i32, i32)> = (0..N).map(|_| (next(), next())).collect();
    let pts: Vec<(i32, i32)> = (0..PROBES).map(|_| (next(), next())).collect();

    let code = |h: RingHit| -> u64 {
        match h {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run = |f: fn(&[(i32, i32)], i32, i32) -> RingHit| -> (u64, u64) {
        let t0 = now_us();
        let mut acc = 0u64; // verdict fingerprint; also defeats elision
        for &(px, py) in &pts {
            acc = acc.wrapping_mul(3).wrapping_add(code(f(&ring, px, py)));
        }
        (now_us() - t0, acc)
    };
    let (t64, a64) = run(ring_hit::<i64, (i32, i32)>);
    let (t128, a128) = run(ring_hit::<i128, (i32, i32)>);
    let (tf64, af64) = run(ring_hit::<f64, (i32, i32)>);
    assert!(a64 == a128 && a64 == af64, "kernel verdicts disagree");
    let edges = (N * PROBES) as u64;
    println!(
        "KERNEL {} edges: i64 {} us ({} ns/edge) · i128 {} us ({:.2}x) · f64 {} us ({:.2}x) · verdicts agree",
        edges,
        t64,
        t64 * 1000 / edges,
        t128,
        t128 as f64 / t64 as f64,
        tf64,
        tf64 as f64 / t64 as f64
    );
}

/// §14.11 follow-up experiment: at i16 quant, can the kernel drop below
/// i64? Sign-magnitude comparison of the two cross products fits u32
/// EXACTLY (|diff| ≤ 65535, and 65535² < 2^32 — the compare form needs 2b
/// bits where the subtract form needs 2b+2), so this races the shipped
/// `ring_hit::<i64, (i16,i16)>` against a u32 sign-split kernel on the
/// identical slice, plus the same geometry widened to `(i32, i32)` pairs
/// for the load-width effect. Full-range i16 ring so worst-case products
/// are exercised; ring-level verdicts must agree exactly (the sign-split
/// kernel may flag Boundary via a different edge of the same vertex, but
/// Boundary short-circuits the ring either way).
fn kernel_bench_i16() {
    use utz::pip::{ring_hit, RingHit};
    const N: usize = 8192;
    const PROBES: usize = 200;
    let mut lcg = 0x0dd_ba11u64;
    let mut next = || {
        lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((lcg >> 48) as u16) as i16 // full i16 range
    };
    let ring16: Vec<(i16, i16)> = (0..N).map(|_| (next(), next())).collect();
    let pts16: Vec<(i16, i16)> = (0..PROBES).map(|_| (next(), next())).collect();
    let ring32: Vec<(i32, i32)> = ring16.iter().map(|&(x, y)| (i32::from(x), i32::from(y))).collect();

    let code = |h: RingHit| -> u64 {
        match h {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        }
    };
    let run16 = |f: fn(&[(i16, i16)], i16, i16) -> RingHit| -> (u64, u64) {
        let t0 = now_us();
        let mut acc = 0u64;
        for &(px, py) in &pts16 {
            acc = acc.wrapping_mul(3).wrapping_add(code(f(&ring16, px, py)));
        }
        (now_us() - t0, acc)
    };
    let run32 = |f: fn(&[(i32, i32)], i32, i32) -> RingHit| -> (u64, u64) {
        let t0 = now_us();
        let mut acc = 0u64;
        for &(px, py) in &pts16 {
            acc = acc.wrapping_mul(3).wrapping_add(code(f(&ring32, i32::from(px), i32::from(py))));
        }
        (now_us() - t0, acc)
    };
    let (t_w32, a_w32) = run32(ring_hit::<i64, (i32, i32)>);
    let (t_n64, a_n64) = run16(ring_hit::<i64, (i16, i16)>);
    let (t_u32, a_u32) = run16(ring_hit16_u32);
    assert!(a_n64 == a_w32 && a_n64 == a_u32, "i16 kernel verdicts disagree");
    let edges = (N * PROBES) as u64;
    println!(
        "KERNEL16 {} edges: i64/i16-pairs {} us ({} ns/edge) · u32-signsplit/i16-pairs {} us ({:.2}x) · i64/i32-pairs {} us ({:.2}x) · verdicts agree",
        edges,
        t_n64,
        t_n64 * 1000 / edges,
        t_u32,
        t_u32 as f64 / t_n64 as f64,
        t_w32,
        t_w32 as f64 / t_n64 as f64
    );
}

/// [`utz::pip::ring_hit`] fold shape over the experimental u32 edge kernel.
fn ring_hit16_u32(ring: &[(i16, i16)], px: i16, py: i16) -> utz::pip::RingHit {
    use utz::pip::{EdgeHit, RingHit};
    let n = ring.len();
    if n < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[n - 1];
    for &(x1, y1) in ring {
        match edge16_u32((x0, y0), (x1, y1), px, py) {
            EdgeHit::Boundary => return RingHit::Boundary,
            EdgeHit::Cross => inside = !inside,
            EdgeHit::Miss => {}
        }
        (x0, y0) = (x1, y1);
    }
    if inside {
        RingHit::Inside
    } else {
        RingHit::Outside
    }
}

/// Experimental 32-bit edge kernel for i16 coords (see `kernel_bench_i16`).
///
/// Normalizes the edge upward first: swapping endpoints negates the cross
/// product, which folds `pip::edge`'s up/down branches into one, and the
/// upward `y1 != py` endpoint-exclusion guard is vacuously true for swapped
/// (originally downward) edges — per-edge Cross verdicts match `pip::edge`
/// exactly; Boundary may fire from a different edge of the same vertex.
/// Then sign(cross) = compare of |dx|·t vs dy·|v| as exact u32 magnitudes
/// plus the operand signs (t, dy ≥ 0 after normalization).
#[inline(always)]
fn edge16_u32(a: (i16, i16), b: (i16, i16), px: i16, py: i16) -> utz::pip::EdgeHit {
    use utz::pip::EdgeHit;
    let ((mut x0, mut y0), (mut x1, mut y1)) = (a, b);
    if y0 > y1 {
        core::mem::swap(&mut x0, &mut x1);
        core::mem::swap(&mut y0, &mut y1);
    }
    if py < y0 || py > y1 {
        return EdgeHit::Miss;
    }
    let dx = i32::from(x1) - i32::from(x0);
    let dy = i32::from(y1) - i32::from(y0); // ≥ 0
    let t = i32::from(py) - i32::from(y0); // 0..=dy
    let v = i32::from(px) - i32::from(x0);
    // cross = dx·t − dy·v; both magnitudes ≤ 65535² < 2^32, exact in u32
    let mag_l = dx.unsigned_abs() * t.unsigned_abs();
    let mag_r = dy.unsigned_abs() * v.unsigned_abs();
    let l_neg = dx < 0 && mag_l != 0;
    let r_neg = v < 0 && mag_r != 0;
    let (gt, eq) = match (l_neg, r_neg) {
        (false, false) => (mag_l > mag_r, mag_l == mag_r),
        (true, true) => (mag_l < mag_r, mag_l == mag_r),
        (true, false) => (false, false), // lhs < 0 ≤ rhs
        (false, true) => (true, false),  // lhs ≥ 0 > rhs
    };
    if eq {
        if x0.min(x1) <= px && px <= x0.max(x1) {
            return EdgeHit::Boundary;
        }
    } else if gt && y1 != py {
        return EdgeHit::Cross; // point strictly left of the upward edge
    }
    EdgeHit::Miss
}
