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

use utz_common::Lcg;

/// A few MB of compressible-but-not-trivial payload: varint-ish noise
/// with runs, deterministic via the workspace LCG.
fn payload() -> Vec<u8> {
    let mut lcg = Lcg::new(0x757a_7472_6970);
    let mut out = Vec::with_capacity(4 << 20);
    while out.len() < 4 << 20 {
        let word = lcg.next_u64();
        // short pseudo-runs make it compress like real section data
        let run = usize::try_from(word & 0x1F).expect("5-bit mask fits") + 1;
        let byte = u8::try_from((word >> 8) & 0x3F).expect("6-bit mask fits");
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
