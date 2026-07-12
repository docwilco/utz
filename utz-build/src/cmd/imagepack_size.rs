//! Does packing `EagerImage` coords to quant width beat general compression
//! of the zero-padded i32 pairs? (§15 — decides whether packed images are
//! worth it for *compressed* assets; uncompressed XIP always saves the raw
//! 25–50%.) Takes v6 geom=2 codec-none containers, rewrites the coords
//! section at quant width, compresses both payloads at preset settings.
//!
//!     utz-build imagepack-size <eager.utz>...

use utz::format::{self, fixed_bytes};
use utz_build::encode::{compress, Codec};

#[derive(clap::Args)]
pub struct Args {
    /// codec-none .utz container path(s)
    #[arg(required = true)]
    paths: Vec<String>,
}

pub fn run(args: &Args) -> utz_build::Result<()> {
    println!(
        "{:<30} {:>9} {:>9} {:>9} {:>9}",
        "image payload", "raw", "gzip", "xz", "brotli"
    );
    for path in &args.paths {
        let bytes = std::fs::read(path)?;
        let (codec, _, start) = format::outer(&bytes).expect("not a utz container");
        assert_eq!(codec, 0, "need codec-none");
        let container_payload = &bytes[start..];
        let header = format::parse(container_payload).unwrap();
        assert_eq!(header.geom, 2, "need an EagerImage container");
        let coord_bytes = fixed_bytes(header.quant_bits);
        let coord_count = header.eager_coords as usize;

        // packed variant: each i32 coord truncated to coord_bytes bytes (LE
        // keeps the low bytes; sign travels in the top retained byte)
        let mut packed = container_payload[..header.img_coords].to_vec();
        for word_idx in 0..coord_count * 2 {
            let word = format::read_u32(container_payload, header.img_coords + word_idx * 4);
            packed.extend_from_slice(&word.to_le_bytes()[..coord_bytes]);
        }
        packed.extend_from_slice(&container_payload[header.img_ring_ends..]);

        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().into_owned();
        for (label, payload) in [
            (format!("{name} i32 pairs"), container_payload.to_vec()),
            (format!("{name} packed i{}", header.quant_bits), packed),
        ] {
            #[expect(clippy::cast_precision_loss, reason = "payload byte counts ≪ 2^53; KiB display")]
            let kib = |len: usize| format!("{:.1}K", len as f64 / 1024.0);
            println!(
                "{:<30} {:>9} {:>9} {:>9} {:>9}",
                label,
                kib(payload.len()),
                kib(compress(&payload, Codec::Gzip)?.len()),
                kib(compress(&payload, Codec::Xz)?.len()),
                kib(compress(&payload, Codec::Brotli)?.len()),
            );
        }
    }
    Ok(())
}
