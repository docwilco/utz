//! Webdist viewer emission: this module writes one static Leaflet page plus
//! binary data blobs per TZBB dataset (arcs and per-vertex densities) and a
//! shared heat raster. Everything is generated on demand, never committed as
//! an asset.

fn template_path(name: &str) -> String {
    format!("{}/templates/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Returns the static webdist viewer page (`webdist_index.html`, with no
/// substitutions), which fetches per-dataset `.bin.z` blobs and
/// `utz_viz.wasm` at runtime.
///
/// # Errors
/// Fails on an I/O error reading the template file.
pub fn webdist_index() -> utz_build::Result<String> {
    Ok(std::fs::read_to_string(template_path(
        "webdist_index.html",
    ))?)
}

/// Builds the binary dataset blob for the webdist viewer (all
/// little-endian). The layout is:
/// `"uTZv" | u32 flags (bit0 = densities, bit1 = topology, bit2 = raw
/// coordinate count) | u32 n_arcs | u32 n_verts | u32 raw_coords (bit2)
/// | u32 offs[n_arcs+1] | pad to 8 | f64 xy[2·n_verts]
/// | f32 dens[n_verts] | topology`. `raw_coords` is the ring-coordinate
/// count before topology dedup, so the viewer's Reduction ladder can show
/// what shared arcs saved.
/// Densities are per-vertex, flat in arc order: each is the max of the
/// vertex's incident edges via `max_along()` (the same edge sampling the
/// builder's weighted path uses), so the browser only maps density to weight
/// (in WASM) and never re-samples geometry.
///
/// The topology section carries everything `payload_from_topology()` needs
/// beyond the arcs, so the viewer can run the asset encoder live (this
/// crate's src/wasm.rs parses it; the JS only reads the prefix above). Its
/// layout is:
/// `u8 dataset_code | u8 rel_len | release bytes | u16 n_features
/// | per feature: f32 offset | u8 len | tzid bytes
/// | u32 n_rings | per ring: u32 nrefs | u32 refs (id<<1|rev)
/// | per feature: u16 npolys | per poly: u16 nrings | u32 ring_idx[nrings]`.
/// Everything is byte-packed with no alignment (the WASM parser reads
/// bytewise).
///
/// # Panics
/// Panics if `release` or any tzid is 256 bytes or longer (they are stored
/// with one-byte lengths).
#[must_use]
pub fn dataset_bin(
    topology: &utz_encode::topo::Topology,
    feats: &[utz_encode::Feat],
    dataset: utz_build::Dataset,
    release: &str,
    grid: Option<&utz_build::density::DensityGrid>,
) -> Vec<u8> {
    let arcs = &topology.arc_coords;
    let n_arcs = arcs.len();
    let n_verts: usize = arcs.iter().map(std::vec::Vec::len).sum();
    let push_u32 = |out: &mut Vec<u8>, value: usize, what: &str| {
        let value = u32::try_from(value).unwrap_or_else(|_| panic!("{what} fits u32"));
        out.extend_from_slice(&value.to_le_bytes());
    };
    let mut out = Vec::with_capacity(24 + 4 * n_arcs + 20 * n_verts);
    out.extend_from_slice(b"uTZv");
    out.extend_from_slice(&(u32::from(grid.is_some()) | 2 | 4).to_le_bytes());
    push_u32(&mut out, n_arcs, "arc count");
    push_u32(&mut out, n_verts, "vert count");
    let raw_coords =
        usize::try_from(crate::coord_count(feats)).expect("raw coordinate count fits usize");
    push_u32(&mut out, raw_coords, "raw coordinate count");
    let mut offset = 0usize;
    push_u32(&mut out, offset, "arc offset");
    for arc in arcs {
        offset += arc.len();
        push_u32(&mut out, offset, "arc offset");
    }
    out.resize(out.len().next_multiple_of(8), 0); // f64 view needs 8-byte alignment
    for arc in arcs {
        for &(x, y) in arc {
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
    }
    if let Some(grid) = grid {
        for arc in arcs {
            let edge_densities: Vec<f64> = arc
                .windows(2)
                .map(|pair| grid.max_along(pair[0], pair[1]))
                .collect();
            for i in 0..arc.len() {
                let left = if i > 0 { edge_densities[i - 1] } else { 0.0 };
                let right = edge_densities.get(i).copied().unwrap_or(0.0);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "density → f32 blob field, rounding is fine"
                )]
                let density = left.max(right) as f32;
                out.extend_from_slice(&density.to_le_bytes());
            }
        }
    }
    // ---- topology section ----
    out.push(dataset as u8);
    assert!(release.len() < 256, "release tag too long");
    out.push(u8::try_from(release.len()).expect("release len fits u8"));
    out.extend_from_slice(release.as_bytes());
    let n_features = u16::try_from(feats.len()).expect("feature count fits u16");
    out.extend_from_slice(&n_features.to_le_bytes());
    for feat in feats {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "offset → f32 blob field, rounding is fine"
        )]
        let offset_f32 = feat.offset as f32;
        out.extend_from_slice(&offset_f32.to_le_bytes());
        let tzid = feat.tzid.as_deref().unwrap_or("");
        assert!(tzid.len() < 256, "tzid too long: {tzid}");
        out.push(u8::try_from(tzid.len()).expect("tzid len fits u8"));
        out.extend_from_slice(tzid.as_bytes());
    }
    out.extend_from_slice(
        &u32::try_from(topology.ring_refs.len())
            .expect("ring count fits u32")
            .to_le_bytes(),
    );
    for refs in &topology.ring_refs {
        out.extend_from_slice(
            &u32::try_from(refs.len())
                .expect("ref count fits u32")
                .to_le_bytes(),
        );
        for &arc_ref in refs {
            out.extend_from_slice(&arc_ref.to_le_bytes());
        }
    }
    for feature_index in 0..feats.len() {
        out.extend_from_slice(
            &u16::try_from(topology.structure[feature_index].len())
                .expect("poly count fits u16")
                .to_le_bytes(),
        );
        for poly in &topology.structure[feature_index] {
            out.extend_from_slice(
                &u16::try_from(poly.len())
                    .expect("ring count fits u16")
                    .to_le_bytes(),
            );
            for &ring_index in poly {
                let ring_index = u32::try_from(ring_index).expect("ring idx fits u32");
                out.extend_from_slice(&ring_index.to_le_bytes());
            }
        }
    }
    out
}

/// Builds the heat raster for the viewer's density layer (little-endian).
/// The layout is:
/// `"uTZh" | u32 w | u32 h | u32 pad | f64 lon0, lat0, dlon, dlat
/// | u8 cells[w·h]`. Cells are the grid max-pooled 4× and log-quantized
/// (0 means unpopulated and draws transparent, 255 ≈ 50k p/km²); the JS
/// reprojects rows to Mercator when drawing.
///
/// # Panics
/// Panics if the binned grid dimensions exceed u32 (not reachable at 4'
/// input).
#[must_use]
pub fn heat_bin(grid: &utz_build::density::DensityGrid) -> Vec<u8> {
    const DOWNSAMPLE: usize = 4;
    let (width, height) = (
        grid.width.div_ceil(DOWNSAMPLE),
        grid.height.div_ceil(DOWNSAMPLE),
    );
    let max_density_ln = 50_000f64.ln();
    let mut cells = vec![0u8; width * height];
    for row in 0..grid.height {
        for col in 0..grid.width {
            let density = f64::from(grid.cells[row * grid.width + col]);
            if density >= 1.0 {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped to 1..=255"
                )]
                let heat = (255.0 * density.ln() / max_density_ln).clamp(1.0, 255.0) as u8;
                let cell = &mut cells[row / DOWNSAMPLE * width + col / DOWNSAMPLE];
                *cell = (*cell).max(heat);
            }
        }
    }
    let mut out = Vec::with_capacity(48 + cells.len());
    out.extend_from_slice(b"uTZh");
    out.extend_from_slice(
        &u32::try_from(width)
            .expect("raster width fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u32::try_from(height)
            .expect("raster height fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(&[0u8; 4]); // pad so the f64 extents sit 8-aligned
    #[expect(clippy::cast_precision_loss, reason = "DOWNSAMPLE = 4, exact in f64")]
    for value in [
        grid.lon0,
        grid.lat0,
        grid.dlon * DOWNSAMPLE as f64,
        grid.dlat * DOWNSAMPLE as f64,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&cells);
    out
}
