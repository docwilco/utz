//! Generates the uncompressed twins of the compact/balanced presets through
//! the consumer builder API (`utz-build` as a build-dependency — the PLAN §11
//! custom-tier path, dogfooded). The preset shapes come from the `utz-data-*`
//! crates via `utz` features; only the codec-none twins, which no data crate
//! ships, are built here. Same recipes as utz-bench-firmware/build.rs so host
//! and target checksums stay comparable.

use utz_build::encode::{Codec, GeomEncoding};
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
    // fixed-width-arc twins: the XIP speed tier (§13/§15 — streaming
    // lookups skip varint decode; costs flash, zero RAM). tiny = i16,
    // compact = i24 (heavier read_fixed byte assembly)
    Config::tiny()
        .codec(Codec::Uncompressed)
        .geom(GeomEncoding::Fixed)
        .out_path(format!("{out}/tiny-fixed-static.utz"))
        .generate()
        .expect("generate tiny-fixed-static.utz");
    Config::compact()
        .codec(Codec::Uncompressed)
        .geom(GeomEncoding::Fixed)
        .out_path(format!("{out}/compact-fixed-none.utz"))
        .generate()
        .expect("generate compact-fixed-none.utz");
    // eager-image twins (§15): the geometry section IS the preload cache —
    // slice kernels run straight off flash (eager speed, zero RAM, no boot)
    Config::tiny()
        .codec(Codec::Uncompressed)
        .geom(GeomEncoding::EagerImage)
        .out_path(format!("{out}/tiny-eager-static.utz"))
        .generate()
        .expect("generate tiny-eager-static.utz");
    Config::compact()
        .codec(Codec::Uncompressed)
        .geom(GeomEncoding::EagerImage)
        .out_path(format!("{out}/compact-eager-static.utz"))
        .generate()
        .expect("generate compact-eager-static.utz");
    // unaligned twin: measures the read_unaligned i24 kernel (fast on
    // permissive cores, the slow path on Xtensa)
    Config::compact()
        .codec(Codec::Uncompressed)
        .geom(GeomEncoding::EagerImage)
        .align_image_rings(false)
        .out_path(format!("{out}/compact-eager-ua.utz"))
        .generate()
        .expect("generate compact-eager-ua.utz");
}
