//! Codec backends (PLAN.md §7). Each cargo feature compiles in the decoder
//! for one outer-header codec byte; a container whose codec is not compiled
//! in fails with [`Error::Decompress`]. Backends:
//!
//! | feature    | codec byte | crate                | needs `std`? |
//! |------------|-----------:|----------------------|--------------|
//! | (always)   | 0          | — (memcpy)           | no           |
//! | `gzip`     | 1          | miniz_oxide          | no (alloc)   |
//! | `ruzstd`   | 2          | ruzstd (pure Rust)   | no (alloc)   |
//! | `zstd-sys` | 2          | zstd (C libzstd)     | yes          |
//! | `brotli`   | 3          | brotli-decompressor  | yes (for now)|
//! | `xz`       | 4          | lzma-rust2           | yes (for now)|
//!
//! If both zstd backends are enabled, `zstd-sys` wins (faster).

use alloc::vec::Vec;

use crate::Error;

/// Decompress `body` into an owned buffer of exactly `raw_len` bytes
/// (`raw_len` comes from the outer header). Codec 0 copies.
#[allow(unused_variables)]
pub fn decompress(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>, Error> {
    let out = match codec {
        0 => body.to_vec(),
        #[cfg(feature = "gzip")]
        1 => {
            // Inflate straight into a raw_len-sized buffer (the outer header
            // gives the exact size): the output slice doubles as the DEFLATE
            // history, so decode RAM is decoded + ~10 K tables.
            // decompress_to_vec_zlib would grow an unhinted Vec instead —
            // realloc overlap peaks at ~1.4× decoded (window-sweep, §7).
            let mut out = alloc::vec![0u8; raw_len];
            let n = miniz_oxide::inflate::decompress_slice_iter_to_slice(
                &mut out,
                core::iter::once(body),
                true,  // zlib header
                false, // verify adler32
            )
            .map_err(|_| Error::Decompress)?;
            out.truncate(n);
            out
        }
        #[cfg(feature = "zstd-sys")]
        2 => zstd::stream::decode_all(body).map_err(|_| Error::Decompress)?,
        #[cfg(all(feature = "ruzstd", not(feature = "zstd-sys")))]
        2 => {
            use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
            use ruzstd::io::Read as _;
            // Drive the decoder block-by-block, draining after each one:
            // decode_all_to_vec batches up to 1 MiB in the internal decode
            // buffer before draining, peaking at ~2× decoded regardless of
            // the frame's window and defeating the window knob (§7). This
            // loop keeps the internal buffer at window + one block.
            let mut input = body;
            let mut dec = FrameDecoder::new();
            dec.init(&mut input).map_err(|_| Error::Decompress)?;
            let mut out = alloc::vec![0u8; raw_len];
            let mut written = 0;
            loop {
                dec.decode_blocks(&mut input, BlockDecodingStrategy::UptoBlocks(1))
                    .map_err(|_| Error::Decompress)?;
                written += dec.read(&mut out[written..]).map_err(|_| Error::Decompress)?;
                if dec.can_collect() != 0 {
                    return Err(Error::BadFormat); // frame holds more than raw_len declared
                }
                if dec.is_finished() {
                    break;
                }
            }
            out.truncate(written);
            out
        }
        #[cfg(feature = "brotli")]
        3 => {
            let mut out = Vec::with_capacity(raw_len);
            brotli_decompressor::BrotliDecompress(&mut &body[..], &mut out)
                .map_err(|_| Error::Decompress)?;
            out
        }
        #[cfg(feature = "xz")]
        4 => {
            use std::io::Read as _;
            let mut out = Vec::with_capacity(raw_len);
            lzma_rust2::XzReader::new(body, false)
                .read_to_end(&mut out)
                .map_err(|_| Error::Decompress)?;
            out
        }
        _ => return Err(Error::Decompress),
    };
    if out.len() != raw_len {
        return Err(Error::BadFormat); // header lied about the payload size
    }
    Ok(out)
}
