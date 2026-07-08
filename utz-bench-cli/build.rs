//! Generates the uncompressed twins of the compact/balanced presets through
//! the consumer builder API (`utz-build` as a build-dependency — the PLAN §11
//! custom-tier path, dogfooded). The preset shapes come from the `utz-data-*`
//! crates via `utz` features; only the codec-none twins, which no data crate
//! ships, are built here. Same recipes as utz-bench-firmware/build.rs so host
//! and target checksums stay comparable.

use utz_build::encode::Codec;
use utz_build::Config;

fn main() {
    // sources are cond-GET-cached in the workspace cache/; the encode reruns
    // only when this recipe changes
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::env::var("OUT_DIR").unwrap();
    Config::compact()
        .codec(Codec::Uncompressed)
        .out_path(format!("{out}/compact-none.utz"))
        .generate()
        .expect("generate compact-none.utz");
    Config::balanced()
        .codec(Codec::Uncompressed)
        .out_path(format!("{out}/balanced-none.utz"))
        .generate()
        .expect("generate balanced-none.utz");
}
