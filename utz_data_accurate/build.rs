//! Recipe guard: refuse to compile against an asset whose header does
//! not match this preset's recipe. Regenerate with
//! `scripts/gen-presets.sh` (or `utz_build_cli gen-preset accurate`).

use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

fn main() {
    println!("cargo:rerun-if-changed=data/accurate.utz");
    let bytes = std::fs::read("data/accurate.utz")
        .expect("data/accurate.utz missing: run scripts/gen-presets.sh");
    let h = PayloadHeader::from_asset(&bytes)
        .expect("data/accurate.utz is not a current-version asset: run scripts/gen-presets.sh");
    let expected = (
        Dataset::All,
        10.0_f32,
        QuantBits::Bits32,
        0.5,
        SimplifyAlgo::Rdp,
        GeomEncoding::VarintArcs,
        Codec::Brotli,
        1000_u16,
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
        "data/accurate.utz does not match the accurate recipe: run scripts/gen-presets.sh"
    );
}
