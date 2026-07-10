// Measurement backlog (PLAN.md §7 + §15): ratio-vs-window sweep + MEASURED
// peak decode RAM. Encodes the real payload per codec across window/dict
// sizes (capped at decoded size), then decodes each blob through the exact
// paths `utz` ships (`utz::decompress` — ruzstd backend here, not zstd-sys)
// under a tracking allocator. Goal: pick preset windows at the ratio knee and
// verify the `peak ≈ decoded + window + state` model.
//
// usage: utz-build window-sweep [ds] [grid_deg] [--eps E [--quant B]]

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use utz_build::encode::{self, Codec, Params};

/// Counts live/peak heap bytes for the whole binary (the other subcommands
/// pay two relaxed atomics per alloc — noise). `realloc` stays at the trait
/// default, which routes through alloc+copy+dealloc on these counters: grow-
/// in-place is deliberately disabled, so peaks include the old+new overlap a
/// naive/embedded allocator would pay.
struct Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(live, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        LIVE.fetch_sub(l.size(), Relaxed);
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

/// Run `f`, returning (result, peak heap growth over entry live, wall ms).
pub(crate) fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, f64) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let t = Instant::now();
    let r = f();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    (r, PEAK.load(Relaxed).saturating_sub(base), ms)
}

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// grid cell size in integer degrees
    #[arg(default_value_t = 2.0)]
    grid_deg: f64,
    /// sweep a single ε (meters) instead of the preset-candidate list
    #[arg(long)]
    eps: Option<f64>,
    /// quant bits for --eps (default 24 if ε≤250 else 16)
    #[arg(long)]
    quant: Option<u32>,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    // preset candidates (§14.5): i16 pairs with ε≥500, i24 with ε≤250 (§15)
    let shapes: Vec<(f64, u32)> = match a.eps {
        Some(e) => vec![(e, a.quant.unwrap_or(if e <= 250.0 { 24 } else { 16 }))],
        None => vec![(2000.0, 16), (1000.0, 16), (500.0, 16), (250.0, 24), (100.0, 24)],
    };
    let feats = utz_build::load(&a.ds)?;

    for (eps_m, qbits) in shapes {
        let p = Params {
            dataset: utz_build::dataset(&a.ds)?.code(),
            tzbb_release: "dev",
            eps_m,
            quant_bits: qbits,
            grid_deg: a.grid_deg,
            codec: Codec::Uncompressed,
            simplify: Default::default(),
            geom: Default::default(),
        };
        let payload = encode::build_payload(&feats, &p)?;
        let raw = payload.len();
        println!(
            "\n{} ε={}m i{qbits}, grid {}° — raw {:.1} K",
            a.ds.to_uppercase(),
            eps_m,
            a.grid_deg,
            raw as f64 / 1024.0
        );
        println!(
            "{:>8}{:>9}{:>10}{:>8}{:>9}{:>10}{:>9}",
            "codec", "window", "comp", "ratio", "dec_ms", "peak", "state"
        );
        println!("{}", "-".repeat(63));

        // decode through the shipped path; peak-RAM model: decoded + window + state
        let row = |name: &str, window: usize, codec: Codec, blob: Vec<u8>| -> anyhow::Result<()> {
            let (out, peak, ms) = measure(|| utz::decompress::decompress(codec as u8, raw, &blob));
            let out = out.map_err(|e| anyhow::anyhow!("{name} decode: {e:?}"))?;
            anyhow::ensure!(out == payload, "{name} roundtrip mismatch");
            let win_eff = window.min(raw); // beyond raw, back-refs can't reach (§7)
            println!(
                "{:>8}{:>9}{:>9.1}K{:>7.1}%{:>9.1}{:>9.1}K{:>8.1}K",
                name,
                fmt_win(window),
                blob.len() as f64 / 1024.0,
                blob.len() as f64 / raw as f64 * 100.0,
                ms,
                peak as f64 / 1024.0,
                (peak as isize - raw as isize - win_eff as isize) as f64 / 1024.0
            );
            std::io::stdout().flush().ok();
            Ok(())
        };

        let cap_log = usize::BITS - (raw.max(2) - 1).leading_zeros(); // ceil(log2(raw))

        // zstd: encode C libzstd q22 with explicit window log (srcSize known,
        // so libzstd itself caps the frame window at the content size);
        // decode = ruzstd (the no_std device path)
        for wlog in 10..=cap_log.min(27) {
            let mut c = zstd::bulk::Compressor::new(22)?;
            c.set_parameter(zstd::stream::raw::CParameter::WindowLog(wlog))?;
            row("zstd22", 1 << wlog, Codec::Zstd, c.compress(&payload)?)?;
        }
        // gzip: DEFLATE window is fixed 32 K — one point, no knob
        let gz = miniz_oxide::deflate::compress_to_vec_zlib(&payload, 10);
        row("gzip10", 32 << 10, Codec::Gzip, gz)?;
        // brotli q11: lgwin 10–24
        for wlog in 10..=cap_log.min(24) {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams {
                quality: 11,
                lgwin: wlog as i32,
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &payload[..], &mut out, &params)?;
            row("br.q11", 1 << wlog, Codec::Brotli, out)?;
        }
        // xz q9(e-ish): dict is a free u32 — pow2 points, plus exact-raw cap
        let xz_dicts = (12..cap_log)
            .map(|l| 1usize << l)
            .chain([raw])
            .collect::<Vec<_>>();
        for dict in xz_dicts {
            use lzma_rust2::Write as _; // no_std lzma-rust2 XzWriter
            let mut opts = lzma_rust2::XzOptions::with_preset(9);
            opts.lzma_options.dict_size = (dict as u32).max(4096);
            opts.lzma_options.nice_len = 273;
            opts.lzma_options.depth_limit = 512;
            // no_std lzma_rust2::Error isn't std::error::Error → stringify
            let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts)
                .map_err(|e| anyhow::anyhow!("xz: {e:?}"))?;
            w.write_all(&payload).map_err(|e| anyhow::anyhow!("xz: {e:?}"))?;
            let blob = w.finish().map_err(|e| anyhow::anyhow!("xz: {e:?}"))?;
            row("xz9", dict, Codec::Xz, blob)?;
        }
    }
    println!("\npeak = measured heap growth during decode (output buffer included;");
    println!("input blob excluded — it lives in flash on-device).");
    println!("state = peak - raw - min(window, raw), i.e. the model's residual.");
    Ok(())
}

fn fmt_win(w: usize) -> String {
    if w >= 1 << 20 && w.is_multiple_of(1 << 20) {
        format!("{}M", w >> 20)
    } else if w >= 1024 && w.is_multiple_of(1024) {
        format!("{}K", w >> 10)
    } else {
        format!("{:.1}K", w as f64 / 1024.0)
    }
}
