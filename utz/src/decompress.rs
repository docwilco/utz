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
        1 => miniz_oxide::inflate::decompress_to_vec_zlib(body).map_err(|_| Error::Decompress)?,
        #[cfg(feature = "zstd-sys")]
        2 => zstd::stream::decode_all(body).map_err(|_| Error::Decompress)?,
        #[cfg(all(feature = "ruzstd", not(feature = "zstd-sys")))]
        2 => {
            let mut out = Vec::with_capacity(raw_len);
            ruzstd::decoding::FrameDecoder::new()
                .decode_all_to_vec(body, &mut out)
                .map_err(|_| Error::Decompress)?;
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
