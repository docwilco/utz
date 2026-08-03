//! Recipe guard: refuse to compile against an asset whose header does
//! not match this preset's recipe. Regenerate with
//! `scripts/gen-presets.sh` (or `utz-build-cli gen-preset balanced`).

use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

fn main() {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the encoder stores grid_deg through the same f64-to-f32 cast; the guard must match it bit-exactly"
    )]
    let grid_deg = (2.0_f64 / 3.0) as f32;
    println!("cargo:rerun-if-changed=data/balanced.utz");
    let bytes = std::fs::read("data/balanced.utz")
        .expect("data/balanced.utz missing: run scripts/gen-presets.sh");
    let h = PayloadHeader::from_asset(&bytes)
        .expect("data/balanced.utz is not a current-version asset: run scripts/gen-presets.sh");
    let expected = (
        Dataset::Now,
        50.0_f32,
        QuantBits::Bits24,
        grid_deg,
        SimplifyAlgo::Rdp,
        GeomEncoding::VarintArcs,
        Codec::Brotli,
        200_u16,
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
        "data/balanced.utz does not match the balanced recipe: run scripts/gen-presets.sh"
    );
}
