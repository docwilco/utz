//! Webdist viewer emission (PLAN.md §12): one static Leaflet page plus binary
//! data blobs per TZBB dataset (arcs + per-vertex densities) and a shared
//! heat raster. Everything is generated on demand — never a committed asset.

fn template_path(name: &str) -> String {
    format!("{}/templates/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// The static webdist viewer page (`webdist_index.html`, no substitutions):
/// fetches per-dataset `.bin.z` blobs + `utz_simplify.wasm` at runtime.
///
/// # Errors
/// I/O failure reading the template file.
pub fn webdist_index() -> crate::Result<String> {
    Ok(std::fs::read_to_string(template_path("webdist_index.html"))?)
}

/// Binary dataset blob for the webdist viewer (all little-endian):
/// `"uTZv" | u32 flags (bit0 = densities, bit1 = topology) | u32 n_arcs
/// | u32 n_verts | u32 offs[n_arcs+1] | pad to 8 | f64 xy[2·n_verts]
/// | f32 dens[n_verts] | topology`.
/// Densities are per-vertex, flat in arc order: max of the vertex's incident
/// edges via `max_along` — the same edge sampling the builder's weighted path
/// uses, so the browser only maps density → weight (in WASM), never
/// re-samples geometry.
///
/// The topology section carries everything `payload_from_topology` needs
/// beyond the arcs, so the viewer can run the container encoder live
/// (utz-encode/src/wasm.rs parses it; the JS only reads the prefix above):
/// `u8 dataset_code | u8 rel_len | release bytes | u16 n_features
/// | per feature: f32 offset | u8 len | tzid bytes
/// | u32 n_rings | per ring: u32 nrefs | u32 refs (id<<1|rev)
/// | per feature: u16 npolys | per poly: u16 nrings | u32 ring_idx[nrings]`
/// — byte-packed, no alignment (the WASM parser reads bytewise).
///
/// # Panics
/// If `release` or any tzid is 256 bytes or longer (they're stored with
/// one-byte lengths).
#[must_use]
pub fn dataset_bin(
    t: &crate::topo::Topology,
    feats: &[crate::Feat],
    dataset_code: u8,
    release: &str,
    g: Option<&crate::density::DensityGrid>,
) -> Vec<u8> {
    let arcs = &t.arc_coords;
    let n_arcs = arcs.len();
    let n_verts: usize = arcs.iter().map(std::vec::Vec::len).sum();
    let mut o = Vec::with_capacity(24 + 4 * n_arcs + 20 * n_verts);
    o.extend_from_slice(b"uTZv");
    o.extend_from_slice(&(u32::from(g.is_some()) | 2).to_le_bytes());
    o.extend_from_slice(&u32::try_from(n_arcs).expect("arc count fits u32").to_le_bytes());
    o.extend_from_slice(&u32::try_from(n_verts).expect("vert count fits u32").to_le_bytes());
    let mut off = 0u32;
    o.extend_from_slice(&off.to_le_bytes());
    for a in arcs {
        off += u32::try_from(a.len()).expect("arc len fits u32");
        o.extend_from_slice(&off.to_le_bytes());
    }
    o.resize(o.len().next_multiple_of(8), 0); // f64 view needs 8-byte alignment
    for a in arcs {
        for &(x, y) in a {
            o.extend_from_slice(&x.to_le_bytes());
            o.extend_from_slice(&y.to_le_bytes());
        }
    }
    if let Some(g) = g {
        for a in arcs {
            let ew: Vec<f64> = a.windows(2).map(|p| g.max_along(p[0], p[1])).collect();
            for i in 0..a.len() {
                let left = if i > 0 { ew[i - 1] } else { 0.0 };
                let right = ew.get(i).copied().unwrap_or(0.0);
                #[expect(clippy::cast_possible_truncation, reason = "density → f32 blob field, rounding is fine")]
                let d = left.max(right) as f32;
                o.extend_from_slice(&d.to_le_bytes());
            }
        }
    }
    // ---- topology section ----
    o.push(dataset_code);
    assert!(release.len() < 256, "release tag too long");
    o.push(u8::try_from(release.len()).expect("release len fits u8"));
    o.extend_from_slice(release.as_bytes());
    o.extend_from_slice(&u16::try_from(feats.len()).expect("feature count fits u16").to_le_bytes());
    for f in feats {
        #[expect(clippy::cast_possible_truncation, reason = "offset → f32 blob field, rounding is fine")]
        let off32 = f.offset as f32;
        o.extend_from_slice(&off32.to_le_bytes());
        let tzid = f.tzid.as_deref().unwrap_or("");
        assert!(tzid.len() < 256, "tzid too long: {tzid}");
        o.push(u8::try_from(tzid.len()).expect("tzid len fits u8"));
        o.extend_from_slice(tzid.as_bytes());
    }
    o.extend_from_slice(&u32::try_from(t.ring_refs.len()).expect("ring count fits u32").to_le_bytes());
    for refs in &t.ring_refs {
        o.extend_from_slice(&u32::try_from(refs.len()).expect("ref count fits u32").to_le_bytes());
        for &r in refs { o.extend_from_slice(&r.to_le_bytes()); }
    }
    for fi in 0..feats.len() {
        o.extend_from_slice(&u16::try_from(t.structure[fi].len()).expect("poly count fits u16").to_le_bytes());
        for poly in &t.structure[fi] {
            o.extend_from_slice(&u16::try_from(poly.len()).expect("ring count fits u16").to_le_bytes());
            for &ri in poly { o.extend_from_slice(&u32::try_from(ri).expect("ring idx fits u32").to_le_bytes()); }
        }
    }
    o
}

/// Heat raster for the viewer's density layer (little-endian):
/// `"uTZh" | u32 w | u32 h | u32 pad | f64 lon0, lat0, dlon, dlat
/// | u8 cells[w·h]` — the grid max-pooled 4× and log-quantized
/// (0 = unpopulated → transparent, 255 ≈ 50k p/km²); the JS reprojects
/// rows to Mercator when drawing.
#[must_use]
///
/// # Panics
/// If the binned grid dimensions exceed u32 (not reachable at 4' input).
pub fn heat_bin(g: &crate::density::DensityGrid) -> Vec<u8> {
    const DS: usize = 4;
    let (w, h) = (g.w.div_ceil(DS), g.h.div_ceil(DS));
    let dmax_ln = 50_000f64.ln();
    let mut cells = vec![0u8; w * h];
    for r in 0..g.h {
        for c in 0..g.w {
            let d = f64::from(g.cells[r * g.w + c]);
            if d >= 1.0 {
                #[expect(clippy::cast_possible_truncation, reason = "clamped to 1..=255")]
                let v = (255.0 * d.ln() / dmax_ln).clamp(1.0, 255.0) as u8;
                let out = &mut cells[r / DS * w + c / DS];
                *out = (*out).max(v);
            }
        }
    }
    let mut o = Vec::with_capacity(48 + cells.len());
    o.extend_from_slice(b"uTZh");
    o.extend_from_slice(&u32::try_from(w).expect("raster width fits u32").to_le_bytes());
    o.extend_from_slice(&u32::try_from(h).expect("raster height fits u32").to_le_bytes());
    o.extend_from_slice(&[0u8; 4]); // pad so the f64 extents sit 8-aligned
    for v in [g.lon0, g.lat0, g.dlon * DS as f64, g.dlat * DS as f64] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&cells);
    o
}
