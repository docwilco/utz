// Regenerate the tuning viewers (PLAN.md §12).
//
// usage: cargo run --release -p utz-build --example visualize [overlay|border|live] [now|1970] [full]
//   overlay — whole-dataset RDP overlay viewer   -> <ds>_overlay.html
//   border  — Portugal/Spain border detail sweep -> border_sweep.html
//   live    — full-res arcs + utz-simplify WASM: algorithm radio + ε slider
//             run the builder's own code in the browser -> <ds>_live.html
//   "full" (overlay only): also embed ε=0 (~73 MB HTML for OSM)

use utz_build::{topo, viz, Feat};

fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "overlay".into());
    let ds = std::env::args().nth(2).unwrap_or_else(|| "now".into());
    let feats = utz_build::load(&ds)?;
    match mode.as_str() {
        "overlay" => overlay(&ds, &feats),
        "border" => border(&feats),
        "live" => live(&ds, &feats),
        m => anyhow::bail!("unknown mode {m:?}: use overlay|border|live"),
    }
}

fn live(ds: &str, feats: &[Feat]) -> anyhow::Result<()> {
    let topo0 = topo::build_topology(feats, 0.0);
    let stored0: usize = topo0.arc_coords.iter().map(|a| a.len()).sum();
    println!("{} arcs, {stored0} full-res verts", topo0.arc_coords.len());

    let wasm = build_wasm()?;
    println!("utz_simplify.wasm: {:.1} KiB", wasm.len() as f64 / 1024.0);

    let sub = format!("{ds} with-oceans · full-res arcs · simplification runs in-browser (utz-simplify WASM)");
    let html = viz::live_html(&topo0.arc_coords, stored0, &base64(&wasm), "OSM time zones — live simplification", &sub)?;
    let outp = format!("{ds}_live.html");
    std::fs::write(&outp, &html)?;
    println!("wrote {outp}  ({:.1} MiB)", html.len() as f64 / (1 << 20) as f64);
    Ok(())
}

/// Build utz-simplify for wasm32-unknown-unknown and return the cdylib bytes.
fn build_wasm() -> anyhow::Result<Vec<u8>> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "utz-simplify", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(root)
        .status()?;
    anyhow::ensure!(status.success(), "wasm build failed — try: rustup target add wasm32-unknown-unknown");
    Ok(std::fs::read(format!("{root}/target/wasm32-unknown-unknown/release/utz_simplify.wasm"))?)
}

fn base64(data: &[u8]) -> String {
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

fn overlay(ds: &str, feats: &[Feat]) -> anyhow::Result<()> {
    let include_full = std::env::args().nth(3).as_deref() == Some("full");
    let mut epslist: Vec<u32> = vec![100, 250, 500, 1000, 2000];
    if include_full { epslist.insert(0, 0); }
    let sub = if include_full {
        "with-oceans · topology-aware RDP · Full = f64 (heavy — toggle on when ready)"
    } else {
        "with-oceans · topology-aware RDP · full omitted — all sets quantizable"
    };

    let sweeps: Vec<(u32, topo::TopoOut)> = epslist.iter()
        .map(|&e| (e, topo::encode_topology(feats, e as f64 / 111_320.0)))
        .collect();
    let levels: Vec<viz::Level> = sweeps.iter()
        .map(|(e, out)| viz::Level { eps_m: *e, feats: out.simplified.iter().collect(), stored: out.verts })
        .collect();
    // ε=0 stored-vertex baseline for the reduction stats (topology only, no encode)
    let stored0: usize = topo::build_topology(feats, 0.0).arc_coords.iter().map(|a| a.len()).sum();
    println!("eps=   0 m  stored={stored0} (baseline)");
    for l in &levels {
        let drawn: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        println!("eps={:>4} m  stored={} drawn={drawn}", l.eps_m, l.stored);
    }

    let html = viz::overlay_html(&levels, &[100, 500], "OSM time zones", sub, stored0)?;
    let outp = format!("{ds}_overlay.html");
    std::fs::write(&outp, &html)?;
    println!("wrote {outp}  ({:.1} MiB)", html.len() as f64 / (1 << 20) as f64);
    Ok(())
}

fn border(feats: &[Feat]) -> anyhow::Result<()> {
    // the two features meeting at the Portugal/Spain land border
    let picks = [("Portugal side", -8.0, 39.5), ("Spain side", -3.7, 40.4)];
    let mut sel: Vec<usize> = Vec::new();
    for (name, lon, lat) in picks {
        if let Some(i) = feats.iter().position(|f| feat_contains(f, lon, lat)) {
            if !sel.contains(&i) { sel.push(i); }
            println!("{name}: feature #{i} tzid={:?}", feats[i].tzid);
        }
    }

    let epslist = [0u32, 25, 50, 100, 250, 500, 1000, 2000];
    let sweeps: Vec<(u32, topo::TopoOut)> = epslist.iter()
        .map(|&e| (e, topo::encode_topology(feats, e as f64 / 111_320.0)))
        .collect();
    let levels: Vec<viz::Level> = sweeps.iter()
        .map(|(e, out)| viz::Level { eps_m: *e, feats: sel.iter().map(|&s| &out.simplified[s]).collect(), stored: 0 })
        .collect();
    for l in &levels {
        let verts: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        println!("eps={:>4} m  verts={verts}", l.eps_m);
    }

    let html = viz::border_html(&levels)?;
    std::fs::write("border_sweep.html", &html)?;
    println!("wrote border_sweep.html ({:.1} KiB)", html.len() as f64 / 1024.0);
    Ok(())
}

fn feat_contains(f: &Feat, lon: f64, lat: f64) -> bool {
    use geo::Contains;
    let pt = geo::Point::new(lon, lat);
    for p in &f.polys {
        if p[0].len() < 3 { continue; }
        let ext = geo::LineString::from(closed(&p[0]));
        let holes: Vec<_> = p[1..].iter().filter(|h| h.len() >= 3).map(|h| geo::LineString::from(closed(h))).collect();
        if geo::Polygon::new(ext, holes).contains(&pt) { return true; }
    }
    false
}

fn closed(v: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut r = v.to_vec();
    if r.first() != r.last() { if let Some(&f) = r.first() { r.push(f); } }
    r
}
