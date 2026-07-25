//! Codec backends. Each cargo feature compiles in the decoder
//! for one outer-header codec byte; a container whose codec is not compiled
//! in fails with [`Error::CodecNotCompiledIn`]. Backends:
//!
//! | feature    | codec byte | crate                | needs `std`? |
//! |------------|-----------:|----------------------|--------------|
//! | (always)   | 0          | — (memcpy)           | no           |
//! | `gzip`     | 1          | [`miniz_oxide`](https://docs.rs/miniz_oxide) | no (alloc)   |
//! | `ruzstd`   | 2          | [`ruzstd`](https://docs.rs/ruzstd) (pure Rust) | no (alloc)   |
//! | `zstd-sys` | 2          | [`zstd`](https://docs.rs/zstd) (C libzstd) | yes          |
//! | `brotli`   | 3          | [`brotli-decompressor`](https://docs.rs/brotli-decompressor) (no-stdlib) | no (alloc) |
//! | `xz`       | 4          | [`lzma-rust2`](https://docs.rs/lzma-rust2) (`no_std`) | no (alloc) |
//!
//! If both zstd backends are enabled, `zstd-sys` wins (faster).

use alloc::vec::Vec;

use crate::{Error, Result};

/// Decompress `body` into an owned buffer of exactly `raw_len` bytes
/// (`raw_len` comes from the outer header). Codec 0 copies.
///
/// # Errors
/// [`Error::CodecNotCompiledIn`] if the codec has no compiled-in backend;
/// [`Error::DecoderFailed`] if the stream is corrupt (carries the backend's
/// own diagnostic); [`Error::RawLengthMismatch`] if the decoded size
/// disagrees with `raw_len`.
pub fn decompress(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>> {
    let out = match codec {
        0 => body.to_vec(),
        #[cfg(feature = "gzip")]
        1 => decompress_gzip(codec, raw_len, body)?,
        #[cfg(feature = "zstd-sys")]
        2 => zstd::stream::decode_all(body)
            .map_err(|source| Error::decoder_failed(codec, format_args!("{source}")))?,
        #[cfg(all(feature = "ruzstd", not(feature = "zstd-sys")))]
        2 => decompress_ruzstd(codec, raw_len, body)?,
        #[cfg(feature = "brotli")]
        3 => decompress_brotli(codec, raw_len, body)?,
        #[cfg(feature = "xz")]
        4 => decompress_xz(codec, raw_len, body)?,
        _ => return Err(Error::CodecNotCompiledIn(codec)),
    };
    if out.len() != raw_len {
        return Err(Error::RawLengthMismatch); // header lied about the payload size
    }
    Ok(out)
}

/// Inflate straight into a `raw_len`-sized buffer: the output slice doubles
/// as the DEFLATE history, so `miniz_oxide` needs no separate 32 KB window and
/// decode RAM is decoded + ~10 K tables. `decompress_to_vec_zlib` would grow
/// an unhinted Vec instead — realloc overlap peaks at ~1.4× decoded
/// (measured by the window-sweep bench).
///
/// Status mapping (best-effort, like the other codecs'):
/// `FailedCannotMakeProgress` = input exhausted mid-stream (truncated);
/// `HasMoreOutput` = the stream outgrows `raw_len` (header lied).
#[cfg(feature = "gzip")]
fn decompress_gzip(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>> {
    use miniz_oxide::inflate::TINFLStatus;
    let mut out = alloc::vec![0u8; raw_len];
    let decoded_len = miniz_oxide::inflate::decompress_slice_iter_to_slice(
        &mut out,
        core::iter::once(body),
        true,  // zlib header
        false, // verify adler32
    )
    .map_err(|status| match status {
        TINFLStatus::FailedCannotMakeProgress => Error::Truncated,
        TINFLStatus::HasMoreOutput => Error::RawLengthMismatch,
        status => Error::decoder_failed(codec, format_args!("{status:?}")),
    })?;
    if decoded_len != raw_len {
        return Err(Error::RawLengthMismatch); // stream shorter than raw_len declared
    }
    Ok(out)
}

/// Drive the pure-Rust zstd decoder block-by-block, draining after each
/// block: `decode_all_to_vec` batches up to 1 MiB in the internal decode
/// buffer before draining, peaking at ~2× decoded regardless of the frame's
/// window and defeating the window knob. This loop keeps the internal buffer
/// at window + one block.
#[cfg(all(feature = "ruzstd", not(feature = "zstd-sys")))]
fn decompress_ruzstd(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>> {
    use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
    use ruzstd::io::Read as _;
    let mut input = body;
    let mut dec = FrameDecoder::new();
    dec.init(&mut input)
        .map_err(|source| Error::decoder_failed(codec, format_args!("{source}")))?;
    let mut out = alloc::vec![0u8; raw_len];
    let mut written = 0;
    loop {
        dec.decode_blocks(&mut input, BlockDecodingStrategy::UptoBlocks(1))
            .map_err(|source| Error::decoder_failed(codec, format_args!("{source}")))?;
        written += dec
            .read(&mut out[written..])
            // We could detect Truncated, but it's a real PITA to catch all
            // cases. Just trust that this provides enough detail.
            .map_err(|source| Error::decoder_failed(codec, format_args!("{source}")))?;
        if dec.can_collect() != 0 {
            return Err(Error::RawLengthMismatch); // frame holds more than raw_len declared
        }
        if dec.is_finished() {
            break;
        }
    }
    if written != raw_len {
        return Err(Error::RawLengthMismatch); // frame shorter than raw_len declared
    }
    Ok(out)
}

/// Decode the whole input into an exact-size output in one call, making the
/// two needs-more statuses terminal: input exhausted mid-frame is a truncated
/// asset, output exhausted means the frame outgrows what `raw_len` declared
/// (both best-effort, like the other codecs'). `ResultFailure` carries the
/// specific libbrotli code from `state.error_code` (`BrotliResult` itself is
/// a bare status).
#[cfg(feature = "brotli")]
fn decompress_brotli(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>> {
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
        BrotliResult::ResultSuccess => {}
        BrotliResult::NeedsMoreInput => return Err(Error::Truncated),
        BrotliResult::NeedsMoreOutput => return Err(Error::RawLengthMismatch),
        BrotliResult::ResultFailure => {
            return Err(Error::decoder_failed(
                codec,
                format_args!("{:?}", state.error_code),
            ))
        }
    }
    if out_off != raw_len {
        return Err(Error::RawLengthMismatch); // frame shorter than raw_len declared
    }
    Ok(out)
}

/// Decode via `lzma-rust2` in `no_std` mode. Its `std` feature must stay OFF
/// tree-wide: with it on, the crate's Read/Write become `pub(crate)`
/// re-exports of `std::io` and the `Read as _` import below breaks.
///
/// A read failing with `Eof` means input exhausted mid-stream — a truncated
/// asset (best-effort, like the other codecs' truncation mapping).
#[cfg(feature = "xz")]
fn decompress_xz(codec: u8, raw_len: usize, body: &[u8]) -> Result<Vec<u8>> {
    use lzma_rust2::Read as _;
    let map_read_error = |source: lzma_rust2::Error| match source {
        lzma_rust2::Error::Eof => Error::Truncated,
        source => Error::decoder_failed(codec, format_args!("{source:?}")),
    };
    let mut out = alloc::vec![0u8; raw_len];
    let mut reader = lzma_rust2::XzReader::new(body, false);
    let mut decoded_len = 0;
    while decoded_len < raw_len {
        match reader
            .read(&mut out[decoded_len..])
            .map_err(map_read_error)?
        {
            0 => break,
            read_len => decoded_len += read_len,
        }
    }
    if decoded_len != raw_len {
        return Err(Error::RawLengthMismatch); // stream shorter than raw_len declared
    }
    if reader.read(&mut [0u8]).map_err(map_read_error)? != 0 {
        return Err(Error::RawLengthMismatch); // stream longer than raw_len declared
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A corrupt stream must surface the backend's own diagnostic, and
    /// Display must compose the codec byte with that detail.
    #[cfg(any(
        feature = "gzip",
        feature = "ruzstd",
        feature = "zstd-sys",
        feature = "brotli",
        feature = "xz"
    ))]
    fn assert_decoder_failed(expected_codec: u8, err: &Error) {
        match err {
            Error::DecoderFailed { codec, detail } => {
                assert_eq!(*codec, expected_codec);
                assert!(!detail.is_empty(), "backend diagnostic must not be empty");
                let text = alloc::format!("{err}");
                assert!(text.contains(&alloc::format!("codec {expected_codec}")));
                assert!(text.contains(detail.as_str()));
            }
            other => panic!("expected DecoderFailed, got {other:?}"),
        }
    }

    #[test]
    fn unknown_codec_reports_its_byte() {
        assert_eq!(decompress(9, 4, &[]), Err(Error::CodecNotCompiledIn(9)));
    }

    #[test]
    fn memcpy_size_lie_is_raw_length_mismatch() {
        assert_eq!(decompress(0, 5, b"abc"), Err(Error::RawLengthMismatch));
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_corrupt_stream_carries_detail() {
        let err = decompress(1, 64, b"not a zlib stream").expect_err("garbage must not inflate");
        assert_decoder_failed(1, &err);
    }

    /// A zlib stream of the same 64 bytes of ASCII text as [`BROTLI_64`]
    /// (produced by Node's zlib.deflateSync).
    #[cfg(feature = "gzip")]
    const ZLIB_64: [u8; 64] = [
        120, 156, 5, 193, 129, 1, 128, 16, 20, 64, 193, 85, 222, 30, 77, 131, 136, 196, 151, 8, 77,
        223, 93, 243, 150, 187, 7, 19, 209, 85, 70, 198, 201, 228, 236, 169, 60, 200, 107, 43, 205,
        91, 46, 245, 45, 118, 57, 54, 138, 50, 145, 180, 208, 50, 25, 161, 121, 92, 248, 1, 252,
        248, 23, 46,
    ];

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_truncated_stream_is_truncated() {
        assert_eq!(decompress(1, 64, &ZLIB_64).map(|out| out.len()), Ok(64));
        assert_eq!(decompress(1, 64, &ZLIB_64[..30]), Err(Error::Truncated));
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn gzip_size_lie_is_raw_length_mismatch() {
        assert_eq!(decompress(1, 63, &ZLIB_64), Err(Error::RawLengthMismatch));
    }

    #[cfg(any(feature = "ruzstd", feature = "zstd-sys"))]
    #[test]
    fn zstd_corrupt_stream_carries_detail() {
        let err = decompress(2, 64, b"not a zstd frame").expect_err("garbage must not decode");
        assert_decoder_failed(2, &err);
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn brotli_corrupt_stream_carries_libbrotli_code() {
        let err = decompress(3, 64, b"not a brotli stream").expect_err("garbage must not decode");
        assert_decoder_failed(3, &err);
        // the detail is the specific libbrotli code, not the bare status
        assert!(alloc::format!("{err}").contains("BROTLI_DECODER_"));
    }

    /// A brotli stream of 64 bytes of ASCII text (produced by Node's
    /// zlib.brotliCompressSync); truncating or under-declaring `raw_len`
    /// exercises the two needs-more statuses.
    #[cfg(feature = "brotli")]
    const BROTLI_64: [u8; 60] = [
        27, 63, 0, 16, 141, 84, 181, 127, 132, 74, 215, 27, 30, 215, 77, 26, 138, 246, 56, 208, 32,
        59, 115, 121, 97, 3, 14, 28, 2, 123, 216, 7, 28, 207, 165, 52, 181, 12, 26, 11, 244, 196,
        23, 34, 96, 193, 133, 51, 248, 37, 127, 70, 111, 159, 233, 163, 220, 56, 132, 4,
    ];

    #[cfg(feature = "brotli")]
    #[test]
    fn brotli_truncated_stream_is_truncated() {
        assert_eq!(decompress(3, 64, &BROTLI_64).map(|out| out.len()), Ok(64));
        assert_eq!(decompress(3, 64, &BROTLI_64[..30]), Err(Error::Truncated));
    }

    #[cfg(feature = "brotli")]
    #[test]
    fn brotli_size_lie_is_raw_length_mismatch() {
        assert_eq!(decompress(3, 63, &BROTLI_64), Err(Error::RawLengthMismatch));
    }

    #[cfg(feature = "xz")]
    #[test]
    fn xz_corrupt_stream_carries_detail() {
        let err = decompress(4, 64, b"not an xz stream").expect_err("garbage must not decode");
        assert_decoder_failed(4, &err);
    }

    /// An xz stream of the same 64 bytes of ASCII text as [`BROTLI_64`]
    /// (produced by the xz command-line tool).
    #[cfg(feature = "xz")]
    const XZ_64: [u8; 120] = [
        253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 2, 0, 33, 1, 22, 0, 0, 0, 116, 47, 229,
        163, 1, 0, 63, 116, 104, 101, 32, 113, 117, 105, 99, 107, 32, 98, 114, 111, 119, 110, 32,
        102, 111, 120, 32, 106, 117, 109, 112, 115, 32, 111, 118, 101, 114, 32, 116, 104, 101, 32,
        108, 97, 122, 121, 32, 100, 111, 103, 59, 32, 112, 97, 99, 107, 32, 109, 121, 32, 98, 111,
        120, 32, 119, 105, 116, 104, 32, 102, 105, 0, 202, 232, 76, 14, 75, 53, 221, 214, 0, 1, 88,
        64, 231, 35, 56, 36, 31, 182, 243, 125, 1, 0, 0, 0, 0, 4, 89, 90,
    ];

    #[cfg(feature = "xz")]
    #[test]
    fn xz_truncated_stream_is_truncated() {
        assert_eq!(decompress(4, 64, &XZ_64).map(|out| out.len()), Ok(64));
        assert_eq!(decompress(4, 64, &XZ_64[..60]), Err(Error::Truncated));
    }
}
