//! The μTZ encoder crate covers everything between loaded features and a
//! `.utz` container: shared-arc topology and simplification (topo),
//! quantization, the grid prefilter (grid), and the delta+varint container
//! serializer with its generic-compression codecs (encode).
//!
//! The crate was split out of `utz_build` so it compiles for
//! wasm32-unknown-unknown: the webdist viewer runs this exact pipeline live
//! for size stats (the wasm surface lives in `utz_viz`).
//! Everything here is pure Rust with no filesystem or network access; the
//! one C-backed codec (zstd) sits behind the `zstd` cargo feature.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod error;
pub use error::{Error, Result};

mod types;
pub use types::*;

pub mod clean;
pub mod encode;
pub mod grid;
pub mod topo;
pub mod validate;
