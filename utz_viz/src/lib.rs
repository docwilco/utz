//! Everything behind the deployed webdist viewer: the whittle stage
//! ladder `utz_dev_cli whittle` prints, the misassignment pricing, the
//! wasm surface the live page runs, and the site emitter.
//!
//! The ladder mirrors the utz crate docs' "How it works" stages: parsed
//! f64 coordinates → shared-arc topology → simplification → quantized +
//! serialized sections → compressed asset. The [`misassign`] module is
//! the accuracy side of the same story: the misassigned-area/population
//! pricing the viewer's simplify worker runs (through the `wasm`
//! exports) and the accuracy command shares natively. The [`emit`]
//! module (default `emit` feature; off for the wasm build) writes the
//! static site: page, per-dataset blobs, heat raster.
//!
//! [`misassign`]: ../utz_viz/misassign/index.html
//! [`emit`]: ../utz_viz/emit/index.html

use utz_encode::encode::PayloadStats;
use utz_encode::topo::Topology;
use utz_encode::Feat;

#[cfg(feature = "emit")]
pub mod emit;
pub mod misassign;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// Serialized-payload section sizes, straight from [`PayloadStats`].
#[derive(Clone, Copy, Debug)]
pub struct Sections {
    pub header: u32,
    pub zones: u32,
    pub arcs: u32,
    pub rings: u32,
    pub grid: u32,
}

/// One preset's trip down the pipeline: counts and byte sizes per stage.
/// Coordinate stages are counts; `*_bytes` helpers price them as f64
/// pairs (16 bytes), the in-memory representation they reduce.
#[derive(Clone, Copy, Debug)]
pub struct StageLadder {
    /// parsed source coordinates (all rings, closing vertex stripped)
    pub coords: u64,
    /// shared-arc topology before simplification
    pub n_arcs: u32,
    pub arc_verts: u64,
    /// after the recipe's (optionally density-weighted) simplification
    pub simplified_verts: u64,
    /// stored after quantization + cleanup
    pub stored_verts: u32,
    pub sections: Sections,
    /// serialized payload bytes (uncompressed)
    pub payload: u64,
    /// full container bytes at the recipe codec
    pub container: u64,
}

impl StageLadder {
    #[must_use]
    pub const fn coords_bytes(&self) -> u64 {
        self.coords * 16
    }
    #[must_use]
    pub const fn arc_bytes(&self) -> u64 {
        self.arc_verts * 16
    }
    #[must_use]
    pub const fn simplified_bytes(&self) -> u64 {
        self.simplified_verts * 16
    }
}

/// Total ring coordinates across a parsed dataset.
#[must_use]
pub fn coord_count(feats: &[Feat]) -> u64 {
    feats
        .iter()
        .flat_map(|f| &f.polys)
        .flat_map(|p| p.iter())
        .map(|r| r.len() as u64)
        .sum()
}

/// Total arc vertices of a topology (shared borders already deduplicated).
#[must_use]
pub fn arc_verts(t: &Topology) -> u64 {
    t.arc_coords.iter().map(|a| a.len() as u64).sum()
}

/// Assemble the ladder from the pipeline's own artifacts: the parsed
/// features, the unsimplified and simplified topologies, the encoder's
/// [`PayloadStats`], and the serialized/compressed byte counts.
#[must_use]
pub fn ladder(
    feats: &[Feat],
    unsimplified: &Topology,
    simplified: &Topology,
    stats: &PayloadStats,
    payload: u64,
    container: u64,
) -> StageLadder {
    StageLadder {
        coords: coord_count(feats),
        n_arcs: u32::try_from(unsimplified.arc_coords.len()).unwrap_or(u32::MAX),
        arc_verts: arc_verts(unsimplified),
        simplified_verts: arc_verts(simplified),
        stored_verts: stats.n_verts,
        sections: Sections {
            header: stats.header,
            zones: stats.zones,
            arcs: stats.arcs,
            rings: stats.rings,
            grid: stats.grid,
        },
        payload,
        container,
    }
}
