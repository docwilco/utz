//! Recipe guard: refuse to compile against an asset whose header does
//! not match this preset's recipe. Regenerate with
//! `scripts/gen-presets.sh` (or `utz_build_cli gen-preset tiny-static`).

use utz_common::{Codec, Dataset, GeomEncoding, PayloadHeader, QuantBits, SimplifyAlgo};

fn main() {
    println!("cargo:rerun-if-changed=data/tiny-static.utz");
    let bytes = std::fs::read("data/tiny-static.utz")
        .expect("data/tiny-static.utz missing: run scripts/gen-presets.sh");
    let header = PayloadHeader::from_asset(&bytes)
        .expect("data/tiny-static.utz is not a current-version asset: run scripts/gen-presets.sh");
    let expected = (
        Dataset::Now,
        10_000.0_f32,
        QuantBits::Bits16,
        2.0,
        SimplifyAlgo::Rdp,
        GeomEncoding::VarintArcs,
        Codec::Uncompressed,
        10_u16,
    );
    let actual = (
        header.dataset,
        header.eps_m,
        header.quant_bits,
        header.grid_deg,
        header.simplify_algo,
        header.geom,
        header.codec,
        header.density_weight_floor_e4,
    );
    assert_eq!(
        actual, expected,
        "data/tiny-static.utz does not match the tiny-static recipe: run scripts/gen-presets.sh"
    );
}
