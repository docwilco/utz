// Regenerate the tuning viewers (PLAN.md §12).
//
// usage: cargo run --release -p utz-build --example visualize [overlay|border] [osm|osm1970] [full]
//   overlay — whole-dataset RDP overlay viewer   -> <ds>_overlay.html
//   border  — Portugal/Spain border detail sweep -> border_sweep.html
//   "full" (overlay only): also embed ε=0 (~73 MB HTML for OSM)

use utz_build::{topo, viz, Feat};

fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "overlay".into());
    let ds = std::env::args().nth(2).unwrap_or_else(|| "osm".into());
    let feats = utz_build::load(&ds)?;
    match mode.as_str() {
        "overlay" => overlay(&ds, &feats),
        "border" => border(&feats),
        m => anyhow::bail!("unknown mode {m:?}: use overlay|border"),
    }
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
        .map(|(e, out)| viz::Level { eps_m: *e, feats: out.simplified.iter().collect() })
        .collect();
    for l in &levels {
        let verts: usize = l.feats.iter().flat_map(|f| &f.polys).flatten().map(|r| r.len()).sum();
        println!("eps={:>4} m  verts={verts}", l.eps_m);
    }

    let html = viz::overlay_html(&levels, &[100, 500], "OSM time zones", sub)?;
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
        .map(|(e, out)| viz::Level { eps_m: *e, feats: sel.iter().map(|&s| &out.simplified[s]).collect() })
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
