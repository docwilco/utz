//! Tuning-viewer HTML emission (PLAN.md §12): RDP levels × quant grids over a
//! Leaflet basemap. Data self-embeds into the template — the HTML is a generated
//! artifact, never a committed asset.

use crate::Feat;

fn template_path(name: &str) -> String {
    format!("{}/templates/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// One sweep level: RDP ε (meters) + the simplified geometry to embed.
pub struct Level<'a> {
    pub eps_m: u32,
    pub feats: Vec<&'a Feat>,
}

/// Full-dataset overlay viewer (`sweep_overlay_template.html`).
/// `defaults_on`: ε levels toggled on at load.
pub fn overlay_html(levels: &[Level], defaults_on: &[u32], title: &str, sub: &str) -> anyhow::Result<String> {
    let mut data = String::from("[");
    for (i, l) in levels.iter().enumerate() {
        let verts: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        if i > 0 { data.push(','); }
        data.push_str(&format!("{{\"eps\":{},\"verts\":{verts},\"geojson\":{}}}", l.eps_m, fc_geojson(&l.feats)));
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
        .replace("__TITLE__", title).replace("__SUB__", sub))
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
