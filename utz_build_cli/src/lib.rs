// docs.rs-only override of the sibling link; see utz/src/lib.rs.
#![cfg_attr(
    on_docsrs,
    doc = "[`utz_build::Config`]: https://docs.rs/utz_build/latest/utz_build/config/struct.Config.html"
)]
#![cfg_attr(on_docsrs, doc = "")]
//! The μTZ asset-generation CLI, shipped as the `utz_build_cli` binary.
//!
//! This crate is the command-line counterpart of a `build.rs` using
//! [`utz_build::Config`]: it writes `.utz` assets for flash partitions,
//! OTA images, and experiments, where a build script is the wrong shape.
//!
//! ```text
//! utz_build_cli gen [ds] [epsilon_m] [--qbits 24] [--grid-deg 2]
//!     [--codec none|gzip|zstd|brotli|xz] [--algorithm rdp|vw|ii|none]
//!     [--geom varint-arcs|fixed-width-arcs|full-rings|coarse]
//!     [--w-min <mult>] [-o out.utz]
//! utz_build_cli gen-preset [tiny|tiny-static|compact|balanced|accurate]
//! ```
//!
//! [`cmd::generate`] is `gen`: it exposes every knob of
//! [`utz_build::Config`], one flag each. [`cmd::gen_preset`] drives the
//! canonical `utz_build::presets` recipe table (every preset when the
//! name is omitted), so preset assets regenerate from a single source
//! of truth. The repo-internal measurement and
//! viewer commands live in the (unpublished) `utz_dev_cli` crate.
//!
//! [`utz_build::Config`]: ../utz_build/config/struct.Config.html
//! [`cmd::generate`]: ../utz_build_cli/cmd/generate/index.html
//! [`cmd::gen_preset`]: ../utz_build_cli/cmd/gen_preset/index.html

pub mod cmd;
