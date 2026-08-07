//! Recipe guard: refuse to compile against an asset whose header does
//! not match this preset's recipe in [`utz_common::presets`]. Regenerate
//! with `scripts/gen-presets.sh` (or `utz_build_cli gen-preset balanced`).

use utz_common::PayloadHeader;
use utz_common::presets::{BALANCED, Provenance};

fn main() {
    let recipe = &BALANCED;
    let asset = format!("data/{}.utz", recipe.name);
    println!("cargo:rerun-if-changed={asset}");
    let bytes = std::fs::read(&asset)
        .unwrap_or_else(|error| panic!("{asset} missing ({error}): run scripts/gen-presets.sh"));
    let header = PayloadHeader::from_asset(&bytes).unwrap_or_else(|| {
        panic!("{asset} is not a current-version asset: run scripts/gen-presets.sh")
    });
    assert_eq!(
        Provenance::from(&header),
        Provenance::from(recipe),
        "{asset} does not match the {} recipe: run scripts/gen-presets.sh",
        recipe.name
    );
}
