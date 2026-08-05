//! Cross-crate codec roundtrips: encode with `utz_encode`'s real
//! compressors, decode with utz's real backends, at a size where
//! window/dictionary parameters matter. The decompress unit tests use
//! baked vectors, so without this nothing catches the encoder declaring
//! parameters (like a zstd window) the reader-side backends refuse.
#![cfg(all(
    feature = "std",
    feature = "gzip",
    feature = "ruzstd",
    feature = "brotli",
    feature = "xz"
))]

use utz_common::{Lcg, PAYLOAD_SEED};

/// 4 MiB: large enough that encoder-declared window and dictionary
/// parameters come into play on the reader side.
const PAYLOAD_SIZE: usize = 4 << 20;

/// A few MB of compressible-but-not-trivial payload: varint-ish noise
/// with runs, deterministic via the workspace LCG.
fn payload() -> Vec<u8> {
    let mut lcg = Lcg::new(PAYLOAD_SEED);
    let mut out = Vec::with_capacity(PAYLOAD_SIZE);
    while out.len() < PAYLOAD_SIZE {
        let bits = lcg.next_u64();
        // short pseudo-runs make it compress like real section data
        let run = usize::try_from(bits & 0x1F).expect("5-bit mask fits") + 1;
        let byte = u8::try_from((bits >> 8) & 0x3F).expect("6-bit mask fits");
        out.extend(std::iter::repeat_n(byte, run));
    }
    out
}

#[test]
fn every_codec_roundtrips_through_the_real_encoder() {
    let raw = payload();
    // utz::Codec and utz_encode's Codec are the same re-exported type
    for codec in [
        utz::Codec::Uncompressed,
        utz::Codec::Gzip,
        utz::Codec::Zstd,
        utz::Codec::Brotli,
        utz::Codec::Xz,
    ] {
        let compressed =
            utz_encode::encode::compress(&raw, codec).expect("encoder side must accept");
        let decoded = utz::decompress::decompress(codec, raw.len(), &compressed)
            .unwrap_or_else(|error| panic!("{codec:?}: reader backend refused: {error}"));
        assert_eq!(decoded, raw, "{codec:?} roundtrip");
    }
}
