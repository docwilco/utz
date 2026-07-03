//! Tuning-viewer HTML emission (PLAN.md §12): RDP levels × quant grids over a
//! Leaflet basemap. Data self-embeds into the template — the HTML is a generated
//! artifact, never a committed asset.

use crate::Feat;

fn template_path(name: &str) -> String {
    format!("{}/templates/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// One sweep level: simplification ε (meters) + the geometry to embed.
pub struct Level<'a> {
    pub eps_m: u32,
    pub feats: Vec<&'a Feat>,
    /// arc vertices actually stored at this level (post topology dedup +
    /// simplification) — drives the reduction stats; 0 = not tracked
    pub stored: usize,
}

/// Full-dataset overlay viewer (`sweep_overlay_template.html`).
/// `defaults_on`: ε levels toggled on at load. `stored0`: stored arc verts at
/// ε=0, the baseline the stats panel computes reduction against.
pub fn overlay_html(levels: &[Level], defaults_on: &[u32], title: &str, sub: &str, stored0: usize) -> anyhow::Result<String> {
    let mut data = String::from("[");
    for (i, l) in levels.iter().enumerate() {
        let verts: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        if i > 0 { data.push(','); }
        data.push_str(&format!("{{\"eps\":{},\"verts\":{verts},\"stored\":{},\"geojson\":{}}}", l.eps_m, l.stored, fc_geojson(&l.feats)));
    }
    data.push(']');
    let mut defaults = String::from("{");
    for (i, e) in defaults_on.iter().enumerate() {
        if i > 0 { defaults.push(','); }
        defaults.push_str(&format!("{e}:true"));
    }
    defaults.push('}');

    let tpl = std::fs::read_to_string(template_path("sweep_overlay_template.html"))?;
    Ok(tpl.replace("/*DATA*/", &data).replace("/*DEFAULTS*/", &defaults)
        .replace("/*STORED0*/", &stored0.to_string())
        .replace("__TITLE__", title).replace("__SUB__", sub))
}

/// Live-simplification viewer (`live_overlay_template.html`): embeds the
/// FULL-RES topology arcs plus the `utz-simplify` WASM module (base64), so an
/// ε slider + algorithm radio rerun the exact builder code in the browser.
/// With a [`DensityGrid`](crate::density::DensityGrid), also embeds per-vertex
/// population densities (for the live weighting toggle + strength slider) and
/// a coarse heat raster (heatmap toggle + opacity slider).
/// Heavy by design (all ε=0 arcs embedded) — it's a local tuning artifact.
pub fn live_html(arcs: &[Vec<(f64, f64)>], stored0: usize, wasm_b64: &str, title: &str, sub: &str, density: Option<&crate::density::DensityGrid>) -> anyhow::Result<String> {
    let mut data = String::with_capacity(arcs.iter().map(|a| a.len()).sum::<usize>() * 20);
    data.push('[');
    for (i, a) in arcs.iter().enumerate() {
        if i > 0 { data.push(','); }
        data.push('[');
        for (j, &(x, y)) in a.iter().enumerate() {
            if j > 0 { data.push(','); }
            data.push_str(&format!("[{},{}]", rd(x), rd(y)));
        }
        data.push(']');
    }
    data.push(']');
    let (dens, heat) = match density {
        Some(g) => (dens_json(arcs, g), heat_json(g)),
        None => ("null".into(), "null".into()),
    };
    let tpl = std::fs::read_to_string(template_path("live_overlay_template.html"))?;
    Ok(tpl.replace("/*ARCS*/", &data)
        .replace("/*STORED0*/", &stored0.to_string())
        .replace("/*WASM*/", wasm_b64)
        .replace("/*DENS*/", &dens)
        .replace("/*HEAT*/", &heat)
        .replace("__TITLE__", title).replace("__SUB__", sub))
}

/// Per-vertex density, flat in arc order (matches the viewer's COORDS layout):
/// max of the vertex's incident edges sampled with `max_along` — the same
/// edge-based sampling the builder's weighted path uses, so the browser only
/// has to map density → weight (in WASM), never re-sample geometry.
fn dens_json(arcs: &[Vec<(f64, f64)>], g: &crate::density::DensityGrid) -> String {
    let mut s = String::with_capacity(arcs.iter().map(|a| a.len()).sum::<usize>() * 4);
    s.push('[');
    let mut first = true;
    for a in arcs {
        let ew: Vec<f64> = a.windows(2).map(|p| g.max_along(p[0], p[1])).collect();
        for i in 0..a.len() {
            let left = if i > 0 { ew[i - 1] } else { 0.0 };
            let right = ew.get(i).copied().unwrap_or(0.0);
            if !first { s.push(','); }
            first = false;
            s.push_str(&format!("{:.0}", left.max(right)));
        }
    }
    s.push(']');
    s
}

/// Heat raster for the viewer's density layer: the grid max-pooled 4× and
/// log-quantized to u8 (0 = unpopulated → transparent, 255 ≈ 50k p/km²),
/// base64-embedded with its geo extents; the JS reprojects rows to Mercator.
fn heat_json(g: &crate::density::DensityGrid) -> String {
    const DS: usize = 4;
    let (w, h) = (g.w.div_ceil(DS), g.h.div_ceil(DS));
    let dmax_ln = 50_000f64.ln();
    let mut bytes = vec![0u8; w * h];
    for r in 0..g.h {
        for c in 0..g.w {
            let d = f64::from(g.cells[r * g.w + c]);
            if d >= 1.0 {
                let v = (255.0 * d.ln() / dmax_ln).clamp(1.0, 255.0) as u8;
                let out = &mut bytes[r / DS * w + c / DS];
                *out = (*out).max(v);
            }
        }
    }
    format!(
        "{{\"w\":{w},\"h\":{h},\"lon0\":{},\"lat0\":{},\"dlon\":{},\"dlat\":{},\"b64\":\"{}\"}}",
        g.lon0, g.lat0, g.dlon * DS as f64, g.dlat * DS as f64, b64(&bytes)
    )
}

/// Plain base64 (no dep; viz artifacts embed WASM + rasters this way).
pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// Border-detail viewer (`london_sweep_template.html`): a few selected features
/// per ε level, e.g. the two sides of a land border.
pub fn border_html(levels: &[Level]) -> anyhow::Result<String> {
    let mut data = String::from("[");
    for (i, l) in levels.iter().enumerate() {
        let verts: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        if i > 0 { data.push(','); }
        data.push_str(&format!("{{\"eps\":{},\"verts\":{verts},\"geojson\":{}}}", l.eps_m, fc_geojson(&l.feats)));
    }
    data.push(']');
    let tpl = std::fs::read_to_string(template_path("london_sweep_template.html"))?;
    Ok(tpl.replace("/*DATA*/", &data))
}

/// Features → GeoJSON FeatureCollection (MultiPolygon per feature, 6-decimal
/// coords, closing vertex restored).
pub fn fc_geojson(feats: &[&Feat]) -> String {
    let mut s = String::from("{\"type\":\"FeatureCollection\",\"features\":[");
    for (i, f) in feats.iter().enumerate() {
        if i > 0 { s.push(','); }
        s.push_str("{\"type\":\"Feature\",\"properties\":{},\"geometry\":{\"type\":\"MultiPolygon\",\"coordinates\":[");
        for (pi, p) in f.polys.iter().enumerate() {
            if pi > 0 { s.push(','); }
            s.push('[');
            for (ri, r) in p.iter().enumerate() {
                if ri > 0 { s.push(','); }
                s.push('[');
                let cl = closed(r);
                for (j, &(x, y)) in cl.iter().enumerate() {
                    if j > 0 { s.push(','); }
                    s.push_str(&format!("[{},{}]", rd(x), rd(y)));
                }
                s.push(']');
            }
            s.push(']');
        }
        s.push_str("]}}");
    }
    s.push_str("]}");
    s
}

fn rd(v: f64) -> String { format!("{:.6}", v).trim_end_matches('0').trim_end_matches('.').to_string() }

fn closed(v: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut r = v.to_vec();
    if r.first() != r.last() { if let Some(&f) = r.first() { r.push(f); } }
    r
}
