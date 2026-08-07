//! This crate holds everything behind the deployed webdist viewer: the
//! misassignment pricing, the wasm surface the live page runs, the site
//! emitter, and the coordinate/vertex counters that `utz_dev_cli
//! whittle` prices its stage ladder with.
//!
//! The [`misassign`] module is the accuracy side of the pipeline story:
//! the misassigned-area/population pricing the viewer's simplify worker
//! runs (through the `wasm` exports) and the accuracy command shares
//! natively. The [`emit`] module (default `emit` feature; off for the
//! wasm build) writes the static site: the page, the per-dataset blobs,
//! and the heat raster.
//!
//! [`misassign`]: ../utz_viz/misassign/index.html
//! [`emit`]: ../utz_viz/emit/index.html

use utz_encode::Feat;
use utz_encode::topo::Topology;

#[cfg(feature = "emit")]
pub mod emit;
pub mod misassign;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// Counts the total ring coordinates across a parsed dataset.
#[must_use]
pub fn coord_count(feats: &[Feat]) -> u64 {
    feats
        .iter()
        .flat_map(|feat| &feat.polys)
        .flat_map(|poly| poly.iter())
        .map(|ring| ring.len() as u64)
        .sum()
}

/// Counts the total arc vertices of a topology (shared borders are already
/// deduplicated).
#[must_use]
pub fn arc_verts(topology: &Topology) -> u64 {
    topology.arc_coords.iter().map(|arc| arc.len() as u64).sum()
}
