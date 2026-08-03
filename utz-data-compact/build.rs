//! Recipe guard: refuse to compile against an asset whose header does
//! not match this preset's recipe. Regenerate with
//! `scripts/gen-presets.sh` (or `utz-build-cli gen-preset compact`).

use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

fn main() {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the encoder stores grid_deg through the same f64-to-f32 cast; the guard must match it bit-exactly"
    )]
    let grid_deg = (4.0_f64 / 3.0) as f32;
    println!("cargo:rerun-if-changed=data/compact.utz");
    let bytes = std::fs::read("data/compact.utz")
        .expect("data/compact.utz missing: run scripts/gen-presets.sh");
    let h = PayloadHeader::from_asset(&bytes)
        .expect("data/compact.utz is not a current-version asset: run scripts/gen-presets.sh");
    let expected = (
        Dataset::Now,
        1_000.0_f32,
        QuantBits::Bits24,
        grid_deg,
        SimplifyAlgo::Rdp,
        GeomEncoding::VarintArcs,
        Codec::Xz,
        10_u16,
    );
    let actual = (
        h.dataset,
        h.eps_m,
        h.quant_bits,
        h.grid_deg,
        h.simplify_algo,
        h.geom,
        h.codec,
        h.density_weight_floor_e4,
    );
    assert_eq!(
        actual, expected,
        "data/compact.utz does not match the compact recipe: run scripts/gen-presets.sh"
    );
}
