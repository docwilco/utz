//! Codec backends (PLAN.md §7). Each cargo feature compiles in the decoder
//! for one outer-header codec byte; a container whose codec is not compiled
//! in fails with [`Error::Decompress`]. Backends:
//!
//! | feature    | codec byte | crate                | needs `std`? |
//! |------------|-----------:|----------------------|--------------|
//! | (always)   | 0          | — (memcpy)           | no           |
//! | `gzip`     | 1          | `miniz_oxide`          | no (alloc)   |
//! | `ruzstd`   | 2          | ruzstd (pure Rust)   | no (alloc)   |
//! | `zstd-sys` | 2          | zstd (C libzstd)     | yes          |
//! | `brotli`   | 3          | brotli-decompressor (no-stdlib) | no (alloc) |
//! | `xz`       | 4          | lzma-rust2 (`no_std`)  | no (alloc)   |
//!
//! If both zstd backends are enabled, `zstd-sys` wins (faster).

use alloc::vec::Vec;

use crate::Error;

/// Decompress `body` into an owned buffer of exactly `raw_len` bytes
/// (`raw_len` comes from the outer header). Codec 0 copies.
///
/// # Errors
/// [`Error::Decompress`] if the codec has no compiled-in backend or the
/// stream is corrupt; [`Error::BadFormat`] if the decoded size disagrees
/// with `raw_len`.
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
            use brotli_decompressor::{BrotliDecompressStream, BrotliResult, BrotliState};
            let mut out = alloc::vec![0u8; raw_len];
            let mut state = BrotliState::new(HeapAlloc, HeapAlloc, HeapAlloc);
            let (mut avail_in, mut in_off) = (body.len(), 0usize);
            let (mut avail_out, mut out_off) = (raw_len, 0usize);
            let mut total = 0usize;
            match BrotliDecompressStream(
                &mut avail_in,
                &mut in_off,
                body,
                &mut avail_out,
                &mut out_off,
                &mut out,
                &mut total,
                &mut state,
            ) {
                // whole input + exact-size output in one call: anything but
                // success (incl. NeedsMoreInput/Output) is corrupt or a
                // raw_len mismatch
                BrotliResult::ResultSuccess => {}
                _ => return Err(Error::Decompress),
            }
            out.truncate(out_off);
            out
        }
        #[cfg(feature = "xz")]
        4 => {
            // lzma-rust2 in no_std mode. Its `std` feature must stay OFF
            // tree-wide: with it on, the crate's Read/Write become
            // pub(crate) re-exports of std::io and this import breaks.
            use lzma_rust2::Read as _;
            let mut out = alloc::vec![0u8; raw_len];
            let mut r = lzma_rust2::XzReader::new(body, false);
            let mut n = 0;
            while n < raw_len {
                match r.read(&mut out[n..]).map_err(|_| Error::Decompress)? {
                    0 => break,
                    k => n += k,
                }
            }
            if n == raw_len && r.read(&mut [0u8]).map_err(|_| Error::Decompress)? != 0 {
                return Err(Error::BadFormat); // stream longer than raw_len declared
            }
            out.truncate(n);
            out
        }
        _ => return Err(Error::Decompress),
    };
    if out.len() != raw_len {
        return Err(Error::BadFormat); // header lied about the payload size
    }
    Ok(out)
}

/// Global-allocator-backed allocator for the no-stdlib brotli decoder —
/// mirrors alloc-stdlib's `StandardAlloc` (zero-initialized cells), which
/// is `std`-only.
#[cfg(feature = "brotli")]
struct HeapAlloc;

#[cfg(feature = "brotli")]
struct HeapCell<T>(alloc::boxed::Box<[T]>);

#[cfg(feature = "brotli")]
impl<T> Default for HeapCell<T> {
    fn default() -> Self {
        HeapCell(Vec::new().into_boxed_slice())
    }
}

#[cfg(feature = "brotli")]
impl<T> brotli_decompressor::SliceWrapper<T> for HeapCell<T> {
    fn slice(&self) -> &[T] {
        &self.0
    }
}

#[cfg(feature = "brotli")]
impl<T> brotli_decompressor::SliceWrapperMut<T> for HeapCell<T> {
    fn slice_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

#[cfg(feature = "brotli")]
impl<T: Clone + Default> brotli_decompressor::Allocator<T> for HeapAlloc {
    type AllocatedMemory = HeapCell<T>;
    fn alloc_cell(&mut self, len: usize) -> HeapCell<T> {
        HeapCell(alloc::vec![T::default(); len].into_boxed_slice())
    }
    fn free_cell(&mut self, _cell: HeapCell<T>) {}
}
