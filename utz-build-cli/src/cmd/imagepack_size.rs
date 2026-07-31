//! Does packing `FullRings` coords to quant width beat general compression
//! of the zero-padded i32 pairs? (Decides whether packed images are
//! worth it for *compressed* assets; uncompressed XIP always saves the raw
//! 25–50%.) Takes uncompressed full-rings assets, rewrites the coords
//! section at quant width, compresses both payloads at preset settings.
//!
//! ```text
//! utz-build-cli imagepack-size <eager.utz>...
//! ```

use utz::format::{self};
use utz_build::encode::{compress, Codec};
use utz_common::GeomEncoding;

#[derive(clap::Args)]
pub struct Args {
    /// codec-none .utz asset path(s)
    #[arg(required = true)]
    paths: Vec<String>,
}

/// # Errors
/// I/O reading an input asset or a compression backend failure.
///
/// # Panics
/// If an input is not a codec-none `FullRings` .utz asset, or its path has
/// no file stem.
pub fn run(args: &Args) -> utz_build::Result<()> {
    println!(
        "{:<30} {:>9} {:>9} {:>9} {:>9}",
        "image payload", "raw", "gzip", "xz", "brotli"
    );
    for path in &args.paths {
        let bytes = std::fs::read(path)?;
        let start = format::outer(&bytes).expect("not a utz container");
        let container_payload = &bytes[start + format::PAYLOAD_HEADER_LEN..];
        let header = format::parse(&bytes[start..]).unwrap();
        assert_eq!(header.codec, utz::Codec::Uncompressed, "need codec-none");
        assert_eq!(
            header.geom,
            GeomEncoding::FullRings,
            "need an FullRings container"
        );
        let coord_bytes = header.quant_bits.bytes();
        let coord_count = header.eager_coords as usize;

        // packed variant: each i32 coord truncated to coord_bytes bytes (LE
        // keeps the low bytes; sign travels in the top retained byte)
        let mut packed = container_payload[..header.full_coords].to_vec();
        for word_idx in 0..coord_count * 2 {
            let word = format::read_u32(container_payload, header.full_coords + word_idx * 4);
            packed.extend_from_slice(&word.to_le_bytes()[..coord_bytes]);
        }
        packed.extend_from_slice(&container_payload[header.full_ring_ends..]);

        let name = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for (label, payload) in [
            (format!("{name} i32 pairs"), container_payload.to_vec()),
            (
                format!("{name} packed i{}", header.quant_bits.bits()),
                packed,
            ),
        ] {
            #[expect(
                clippy::cast_precision_loss,
                reason = "payload byte counts ≪ 2^53; KiB display"
            )]
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
