// Generate the webdist viewer (PLAN.md §12): one static page + binary data
// files per TZBB dataset, servable from any static host (GitHub Pages,
// `python3 -m http.server -d webdist`). Supersedes the old per-dataset
// embedded viewers (_overlay/_live/border_sweep).
//
// usage: cargo run --release -p utz-build --example visualize [outdir]
//   writes outdir (default webdist/): index.html · utz_simplify.wasm ·
//   heat.bin.z · <dataset>.bin.z for now/1970/all/land-now/land-1970/land-all

use std::path::{Path, PathBuf};

use utz_build::{topo, viz};

const DATASETS: [&str; 6] = ["now", "1970", "all", "land-now", "land-1970", "land-all"];

fn main() -> anyhow::Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "webdist".into()));
    std::fs::create_dir_all(&out)?;

    let wasm = build_wasm()?;
    std::fs::write(out.join("utz_simplify.wasm"), &wasm)?;
    std::fs::write(out.join("index.html"), viz::webdist_index()?)?;
    println!("index.html + utz_simplify.wasm ({:.1} KiB)", wasm.len() as f64 / 1024.0);

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
        let verts: usize = topo0.arc_coords.iter().map(|a| a.len()).sum();
        let bin = viz::dataset_bin(&topo0.arc_coords, dens.as_ref());
        let z = write_z(&out.join(format!("{ds}.bin.z")), &bin)?;
        println!(
            "{ds}: {} arcs, {verts} verts, {:.1} MiB -> {:.1} MiB",
            topo0.arc_coords.len(),
            bin.len() as f64 / (1 << 20) as f64,
            z as f64 / (1 << 20) as f64
        );
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// zlib-deflate (the browser side inflates with `DecompressionStream('deflate')`).
fn write_z(path: &Path, data: &[u8]) -> anyhow::Result<usize> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    std::fs::write(path, &z)?;
    Ok(z.len())
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
