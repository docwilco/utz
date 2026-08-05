//! Sweeps compression ratio against window size and *measures* peak
//! decode RAM. The command encodes the real payload per codec across
//! window/dict sizes (capped at decoded size), then decodes each blob
//! through the exact paths `utz` ships (`utz::decompress()`, with the
//! ruzstd backend here, not zstd-sys) under a tracking allocator. The
//! goal is to pick preset windows at the ratio knee and verify the
//! `peak ≈ decoded + window + state` model.
//!
//! ```text
//! utz_dev_cli window-sweep [ds] [grid_deg] [--epsilon E [--quant B]]
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use utz_build::{ensure, Error};
use utz_encode::encode::{self, Codec, Params};

/// Counts live/peak heap bytes for the whole binary (the other subcommands
/// pay two relaxed atomics per alloc, which is noise). `realloc` stays at
/// the trait default, which routes through alloc+copy+dealloc on these
/// counters: grow-in-place is deliberately disabled, so peaks include the
/// old+new overlap a naive/embedded allocator would pay.
struct Tracking;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = System.alloc(layout);
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(live, Relaxed);
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
        LIVE.fetch_sub(layout.size(), Relaxed);
    }
}

#[global_allocator]
static ALLOC: Tracking = Tracking;

/// Runs `work`, returning (result, peak heap growth over entry live, wall ms).
pub(crate) fn measure<T>(work: impl FnOnce() -> T) -> (T, usize, f64) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let timer = Instant::now();
    let result = work();
    let ms = timer.elapsed().as_secs_f64() * 1e3;
    (result, PEAK.load(Relaxed).saturating_sub(base), ms)
}

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The grid cell size in integer degrees.
    #[arg(default_value_t = 2.0)]
    grid_deg: f64,
    /// Sweeps a single ε (meters) instead of the preset-candidate list.
    #[arg(long)]
    epsilon: Option<f64>,
    /// The quant bits for --epsilon (the default is 24 if ε≤250, else 16).
    #[arg(long)]
    quant: Option<u32>,
}

/// # Errors
/// The command fails on a dataset load/parse or payload encode failure,
/// on a codec backend compress/decode failure, or on a decode that does
/// not round-trip the payload.
///
/// # Panics
/// The command panics if an xz dictionary size (capped at the raw payload
/// size) exceeds `u32`.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: &Args) -> utz_build::Result<()> {
    // preset candidates: i16 pairs with ε≥500, i24 with ε≤250
    let shapes: Vec<(f64, u32)> = match args.epsilon {
        Some(epsilon) => vec![(
            epsilon,
            args.quant.unwrap_or(if epsilon <= 250.0 { 24 } else { 16 }),
        )],
        None => vec![
            (2000.0, 16),
            (1000.0, 16),
            (500.0, 16),
            (250.0, 24),
            (100.0, 24),
        ],
    };
    let features = utz_build::load(&args.ds)?;

    for (epsilon_m, quant_bits) in shapes {
        let params = Params {
            dataset: utz_build::dataset(&args.ds)?,
            tzbb_release: "dev",
            epsilon_m,
            quant_bits,
            grid_deg: args.grid_deg,
            codec: Codec::Uncompressed,
            simplify: encode::SimplifyAlgorithm::default(),
            geom: encode::GeomEncoding::default(),
            density_weight_floor: None,
        };
        let payload = encode::build_payload(&features, &params)?;
        let raw = payload.len();
        #[expect(
            clippy::cast_precision_loss,
            reason = "raw payload size ≪ 2^53; KiB display"
        )]
        let raw_kb = raw as f64 / 1024.0;
        println!(
            "\n{} ε={}m i{quant_bits}, grid {}° — raw {raw_kb:.1} K",
            args.ds.to_uppercase(),
            epsilon_m,
            args.grid_deg
        );
        println!(
            "{:>8}{:>9}{:>10}{:>8}{:>9}{:>10}{:>9}",
            "codec", "window", "comp", "ratio", "dec_ms", "peak", "state"
        );
        println!("{}", "-".repeat(63));

        // decode through the shipped path; peak-RAM model: decoded + window + state
        let row =
            |name: &str, window: usize, codec: Codec, blob: Vec<u8>| -> utz_build::Result<()> {
                let (out, peak, ms) = measure(|| utz::decompress::decompress(codec, raw, &blob));
                let out = out.map_err(|error| Error::Msg(format!("{name} decode: {error:?}")))?;
                ensure!(
                    out == payload,
                    Error::Msg(format!("{name} roundtrip mismatch"))
                );
                let effective_window = window.min(raw); // beyond raw, back-refs can't reach
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "blob/raw/peak byte sizes ≪ 2^53; KiB and ratio display"
                )]
                let (comp_kb, ratio, peak_kb, state_kb) = (
                    blob.len() as f64 / 1024.0,
                    blob.len() as f64 / raw as f64 * 100.0,
                    peak as f64 / 1024.0,
                    (peak.cast_signed() - raw.cast_signed() - effective_window.cast_signed())
                        as f64
                        / 1024.0,
                );
                println!(
                    "{:>8}{:>9}{comp_kb:>9.1}K{ratio:>7.1}%{:>9.1}{peak_kb:>9.1}K{state_kb:>8.1}K",
                    name,
                    fmt_win(window),
                    ms
                );
                std::io::stdout().flush().ok();
                Ok(())
            };

        let cap_log = usize::BITS - (raw.max(2) - 1).leading_zeros(); // ceil(log2(raw))

        // zstd: encode C libzstd q22 with explicit window log (srcSize known,
        // so libzstd itself caps the frame window at the content size);
        // decode = ruzstd (the no_std device path)
        for wlog in 10..=cap_log.min(27) {
            let mut compressor = zstd::bulk::Compressor::new(22)?;
            compressor.set_parameter(zstd::stream::raw::CParameter::WindowLog(wlog))?;
            row(
                "zstd22",
                1 << wlog,
                Codec::Zstd,
                compressor.compress(&payload)?,
            )?;
        }
        // gzip: DEFLATE window is fixed 32 K — one point, no knob
        let gz = miniz_oxide::deflate::compress_to_vec_zlib(&payload, 10);
        row("gzip10", 32 << 10, Codec::Gzip, gz)?;
        // brotli q11: lgwin 10–24
        for wlog in 10..=cap_log.min(24) {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams {
                quality: 11,
                lgwin: wlog.cast_signed(),
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &payload[..], &mut out, &params)?;
            row("br.q11", 1 << wlog, Codec::Brotli, out)?;
        }
        // xz q9(e-ish): dict is a free u32 — pow2 points, plus exact-raw cap
        let xz_dicts = (12..cap_log)
            .map(|log| 1usize << log)
            .chain([raw])
            .collect::<Vec<_>>();
        for dict in xz_dicts {
            use lzma_rust2::Write as _; // no_std lzma-rust2 XzWriter
            let mut options = lzma_rust2::XzOptions::with_preset(9);
            options.lzma_options.dict_size = u32::try_from(dict).expect("dict fits u32").max(4096);
            options.lzma_options.nice_len = 273;
            options.lzma_options.depth_limit = 512;
            // no_std lzma_rust2::Error isn't std::error::Error → stringify
            let mut writer = lzma_rust2::XzWriter::new(Vec::new(), options)
                .map_err(|error| Error::Msg(format!("xz: {error:?}")))?;
            writer
                .write_all(&payload)
                .map_err(|error| Error::Msg(format!("xz: {error:?}")))?;
            let blob = writer
                .finish()
                .map_err(|error| Error::Msg(format!("xz: {error:?}")))?;
            row("xz9", dict, Codec::Xz, blob)?;
        }
    }
    println!("\npeak = measured heap growth during decode (output buffer included;");
    println!("input blob excluded — it lives in flash on-device).");
    println!("state = peak - raw - min(window, raw), i.e. the model's residual.");
    Ok(())
}

fn fmt_win(window: usize) -> String {
    if window >= 1 << 20 && window.is_multiple_of(1 << 20) {
        format!("{}M", window >> 20)
    } else if window >= 1024 && window.is_multiple_of(1024) {
        format!("{}K", window >> 10)
    } else {
        #[expect(
            clippy::cast_precision_loss,
            reason = "window size ≪ 2^53; KiB display"
        )]
        let kib = window as f64 / 1024.0;
        format!("{kib:.1}K")
    }
}
