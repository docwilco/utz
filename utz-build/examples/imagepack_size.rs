//! Does packing `EagerImage` coords to quant width beat general compression
//! of the zero-padded i32 pairs? (§15 — decides whether packed images are
//! worth it for *compressed* assets; uncompressed XIP always saves the raw
//! 25–50%.) Takes v6 geom=2 codec-none containers, rewrites the coords
//! section at quant width, compresses both payloads at preset settings.
//!
//!     cargo run --release -p utz-build --example imagepack_size -- <eager.utz>...

use utz::format::{self, fixed_bytes};
use utz_build::encode::{compress, Codec};

fn main() {
    println!(
        "{:<30} {:>9} {:>9} {:>9} {:>9}",
        "image payload", "raw", "gzip", "xz", "brotli"
    );
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (codec, _, start) = format::outer(&bytes).expect("not a utz container");
        assert_eq!(codec, 0, "need codec-none");
        let p = &bytes[start..];
        let h = format::parse(p).unwrap();
        assert_eq!(h.geom, 2, "need an EagerImage container");
        let fb = fixed_bytes(h.quant_bits);
        let n = h.eager_coords as usize;

        // packed variant: each i32 coord truncated to fb bytes (LE keeps the
        // low bytes; sign travels in the top retained byte)
        let mut packed = p[..h.img_coords].to_vec();
        for i in 0..n * 2 {
            let v = format::read_u32(p, h.img_coords + i * 4);
            packed.extend_from_slice(&v.to_le_bytes()[..fb]);
        }
        packed.extend_from_slice(&p[h.img_ring_ends..]);

        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().into_owned();
        for (label, payload) in [
            (format!("{name} i32 pairs"), p.to_vec()),
            (format!("{name} packed i{}", h.quant_bits), packed),
        ] {
            let k = |x: usize| format!("{:.1}K", x as f64 / 1024.0);
            println!(
                "{:<30} {:>9} {:>9} {:>9} {:>9}",
                label,
                k(payload.len()),
                k(compress(&payload, Codec::Gzip).len()),
                k(compress(&payload, Codec::Xz).len()),
                k(compress(&payload, Codec::Brotli).len()),
            );
        }
    }
}
