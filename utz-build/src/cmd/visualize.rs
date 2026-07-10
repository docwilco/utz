// Generate the webdist viewer (PLAN.md §12): one static page + binary data
// files per TZBB dataset, servable from any static host (GitHub Pages,
// `python3 -m http.server -d webdist`). Supersedes the old per-dataset
// embedded viewers (_overlay/_live/border_sweep).
//
// usage: utz-build visualize [outdir]
//   writes outdir (default webdist/): index.html · utz_encode.wasm ·
//   heat.bin.z · <dataset>.bin.z + zones-<dataset>.bin.z for
//   now/1970/all/land-now/land-1970/land-all

use std::path::{Path, PathBuf};

use utz_build::encode::{self, Codec, Params};
use utz_build::{topo, viz};

const DATASETS: [&str; 6] = ["now", "1970", "all", "land-now", "land-1970", "land-all"];

#[derive(clap::Args)]
pub struct Args {
    /// output directory for the viewer site
    #[arg(default_value = "webdist")]
    out: PathBuf,
}

pub fn run(a: Args) -> anyhow::Result<()> {
    let out = a.out;
    std::fs::create_dir_all(&out)?;

    let wasm = build_wasm()?;
    std::fs::write(out.join("utz_encode.wasm"), &wasm)?;
    std::fs::write(out.join("index.html"), viz::webdist_index()?)?;
    println!("index.html + utz_encode.wasm ({:.1} KiB)", wasm.len() as f64 / 1024.0);

    // density is optional: first use downloads GHS-POP (~460 MB, then cached)
    let dens = match utz_build::density::DensityGrid::load(&utz_build::cache_dir()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("warning: density grid unavailable ({e}); density features disabled");
            None
        }
    };
    if let Some(g) = &dens {
        let n = write_z(&out.join("heat.bin.z"), &viz::heat_bin(g))?;
        println!("heat.bin.z: {:.1} KiB", n as f64 / 1024.0);
    }

    for ds in DATASETS {
        let feats = utz_build::load(ds)?;
        let topo0 = topo::build_topology(&feats, 0.0);
        let verts: usize = topo0.arc_coords.iter().map(std::vec::Vec::len).sum();
        let code = utz_build::dataset(ds)?.code();
        let bin = viz::dataset_bin(&topo0, &feats, code, "webdist", dens.as_ref());
        let z = write_z(&out.join(format!("{ds}.bin.z")), &bin)?;
        let zn = write_z(&out.join(format!("zones-{ds}.bin.z")), &zones_bin(&feats, ds)?)?;
        println!(
            "{ds}: {} arcs, {verts} verts, {:.1} MiB -> {:.1} MiB (+ zones {:.1} MiB)",
            topo0.arc_coords.len(),
            bin.len() as f64 / f64::from(1 << 20),
            z as f64 / f64::from(1 << 20),
            zn as f64 / f64::from(1 << 20)
        );
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// Zone lattice for the coarse-prefilter dominance view: encode a fine-ε
/// container and let the *runtime* answer a 0.1° lattice, so the browser
/// shows exactly what the shipped grid+PIP would answer. Format
/// (little-endian): `"uTZz" | u32 w | u32 h | u32 n_zones
/// | per zone: u16 len + utf8 tzid | pad to 2 | u16 ids[w·h]`
/// (0xFFFF = no zone; row 0 = 90°N, col 0 = 180°W, cell centers sampled).
fn zones_bin(feats: &[utz_build::Feat], ds: &str) -> anyhow::Result<Vec<u8>> {
    const STEP: f64 = 0.1;
    let p = Params {
        dataset: utz_build::dataset(ds)?.code(),
        tzbb_release: "webdist",
        eps_m: 100.0,
        quant_bits: 24,
        grid_deg: 2.0,
        codec: Codec::Zstd,
        simplify: Default::default(),
        geom: Default::default(),
    };
    let finder = utz::Finder::from_vec(encode::encode(feats, &p)?)
        .map_err(|e| anyhow::anyhow!("finder: {e}"))?;
    let mut names: Vec<&str> = feats.iter().filter_map(|f| f.tzid.as_deref()).collect();
    names.sort_unstable();
    names.dedup();
    let idx: std::collections::HashMap<&str, u16> =
        names.iter().enumerate().map(|(i, &n)| (n, i as u16)).collect();

    let (w, h) = ((360.0 / STEP) as usize, (180.0 / STEP) as usize);
    let mut o = Vec::with_capacity(16 + w * h * 2);
    o.extend_from_slice(b"uTZz");
    o.extend_from_slice(&(w as u32).to_le_bytes());
    o.extend_from_slice(&(h as u32).to_le_bytes());
    o.extend_from_slice(&(names.len() as u32).to_le_bytes());
    for n in &names {
        o.extend_from_slice(&(n.len() as u16).to_le_bytes());
        o.extend_from_slice(n.as_bytes());
    }
    o.resize(o.len().next_multiple_of(2), 0);
    for r in 0..h {
        let lat = 90.0 - (r as f64 + 0.5) * STEP;
        for c in 0..w {
            let lon = -180.0 + (c as f64 + 0.5) * STEP;
            let id = finder.lookup(utz::Position { lon, lat }).and_then(|t| idx.get(t).copied()).unwrap_or(0xFFFF);
            o.extend_from_slice(&id.to_le_bytes());
        }
    }
    Ok(o)
}

/// zlib-deflate (the browser side inflates with `DecompressionStream('deflate')`).
fn write_z(path: &Path, data: &[u8]) -> anyhow::Result<usize> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    std::fs::write(path, &z)?;
    Ok(z.len())
}

/// Build utz-encode (simplify + live container encode) for
/// wasm32-unknown-unknown and return the cdylib bytes.
fn build_wasm() -> anyhow::Result<Vec<u8>> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    // cdylib requested here rather than in utz-encode's crate-type — see the
    // [lib] comment in utz-encode/Cargo.toml
    let status = std::process::Command::new("cargo")
        .args(["rustc", "-p", "utz-encode", "--release", "--target", "wasm32-unknown-unknown", "--crate-type", "cdylib"])
        .current_dir(root)
        .status()?;
    anyhow::ensure!(status.success(), "wasm build failed — try: rustup target add wasm32-unknown-unknown");
    Ok(std::fs::read(format!("{root}/target/wasm32-unknown-unknown/release/utz_encode.wasm"))?)
}
