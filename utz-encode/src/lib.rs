//! μTZ encoder crate — everything between loaded features and a `.utz`
//! container: shared-arc topology + simplification (topo), quantization,
//! grid prefilter (grid), and the delta+varint container serializer with its
//! generic-compression codecs (encode).
//!
//! Split out of utz-build so it compiles for wasm32-unknown-unknown: the
//! webdist viewer runs this exact pipeline live for size stats (see wasm.rs).
//! Everything here is pure Rust with no filesystem/network access; the one
//! C-backed codec (zstd) sits behind the `zstd` cargo feature.

mod types;
pub use types::*;

pub mod clean;
pub mod encode;
pub mod grid;
pub mod topo;

#[cfg(target_arch = "wasm32")]
pub mod wasm;
