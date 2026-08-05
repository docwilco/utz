//! Generates the webdist viewer: one static page plus binary data files
//! per TZBB dataset, servable from any static host (GitHub Pages,
//! `python3 -m http.server -d webdist`).
//!
//! ```text
//! utz_dev_cli visualize [outdir]
//!   writes outdir (default webdist/): index.html · utz_viz.wasm ·
//!   heat.bin.z · <dataset>.bin.z + zones-<dataset>.bin.z for
//!   now/1970/all/land-now/land-1970/land-all
//! ```

use std::path::{Path, PathBuf};

use utz_build::{ensure, Error};
use utz_encode::encode::{self, Codec, Params};
use utz_encode::topo;
use utz_viz::emit as viz;

const DATASETS: [&str; 6] = ["now", "1970", "all", "land-now", "land-1970", "land-all"];

#[derive(clap::Args)]
pub struct Args {
    /// The output directory for the viewer site.
    #[arg(default_value = "webdist")]
    out: PathBuf,
    /// Regenerates every blob even if its inputs are unchanged.
    #[arg(long)]
    force: bool,
}

/// # Errors
/// The command fails on a wasm viewer build failure, a dataset or
/// density-grid download/parse failure, or an I/O error writing the site
/// files.
pub fn run(args: Args) -> utz_build::Result<()> {
    let out_dir = args.out;
    std::fs::create_dir_all(&out_dir)?;

    let wasm = build_wasm()?;
    std::fs::write(out_dir.join("utz_viz.wasm"), &wasm)?;
    std::fs::write(out_dir.join("index.html"), viz::webdist_index()?)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "wasm blob size ≪ 2^53; KiB display"
    )]
    let wasm_kib = wasm.len() as f64 / 1024.0;
    println!("index.html + utz_viz.wasm ({wasm_kib:.1} KiB)");

    // Blob cache: each output carries a `.stamp-*` of the inputs that shaped
    // it — this binary, the TZBB zip (whose conditional-GET validators only
    // change when the content does), and the density sidecar. Matching stamp
    // + present outputs → skip the GeoJSON parse / lattice sampling /
    // deflate entirely.
    let cache = utz_build::cache_dir();
    let exe_fingerprint =
        std::env::current_exe().map_or_else(|_| "unknown".into(), |path| hash_file(&path));
    let release = utz_build::loader::resolve_release(&cache)?;
    let density_fingerprint = file_fp(&utz_build::density::sidecar_path(&cache));

    // density loads lazily: an all-fresh run touches neither the ~58 MB
    // sidecar nor the ~460 MB GHS-POP download behind its first build
    let mut density = LazyDensity::Unprobed;

    let heat_path = out_dir.join("heat.bin.z");
    let heat_stamp = format!("v1\nexe:{exe_fingerprint}\ndens:{density_fingerprint}\n");
    let heat_stamp_path = out_dir.join(".stamp-heat");
    if !args.force && fresh(&heat_stamp_path, &heat_stamp, &[&heat_path]) {
        println!("heat.bin.z: cached (inputs unchanged)");
    } else if let Some(grid) = density.get() {
        let deflated_len = write_z(&heat_path, &viz::heat_bin(grid))?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "deflated heatmap size ≪ 2^53; KiB display"
        )]
        let heat_kib = deflated_len as f64 / 1024.0;
        println!("heat.bin.z: {heat_kib:.1} KiB");
        std::fs::write(&heat_stamp_path, &heat_stamp)?;
    }

    for dataset in DATASETS {
        let dataset_kind = utz_build::dataset(dataset)?;
        let zip = utz_build::download::fetch(
            &utz_build::loader::dataset_url(dataset_kind, &release),
            &cache,
        )?;
        let stamp = format!(
            "v1\nexe:{exe_fingerprint}\nrelease:{release}\nzip:{}\ndens:{density_fingerprint}\n",
            zip_fp(&zip)
        );
        let stamp_path = out_dir.join(format!(".stamp-{dataset}"));
        let bin_path = out_dir.join(format!("{dataset}.bin.z"));
        let zones_path = out_dir.join(format!("zones-{dataset}.bin.z"));
        if !args.force && fresh(&stamp_path, &stamp, &[&bin_path, &zones_path]) {
            println!("{dataset}: cached (inputs unchanged)");
            continue;
        }
        let features = utz_build::loader::load_geojson_zip(&zip)?;
        let topo0 = topo::build_topology(&features, 0.0);
        let verts: usize = topo0.arc_coords.iter().map(std::vec::Vec::len).sum();
        let bin = viz::dataset_bin(
            &topo0,
            &features,
            dataset_kind.code(),
            "webdist",
            density.get(),
        );
        let bin_z = write_z(&bin_path, &bin)?;
        let zones_z = write_z(&zones_path, &zones_bin(&features, dataset)?)?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "raw/deflated bin sizes ≪ 2^53; MiB display"
        )]
        let (raw_mib, bin_z_mib, zones_z_mib) = (
            bin.len() as f64 / f64::from(1 << 20),
            bin_z as f64 / f64::from(1 << 20),
            zones_z as f64 / f64::from(1 << 20),
        );
        println!(
            "{dataset}: {} arcs, {verts} verts, {raw_mib:.1} MiB -> {bin_z_mib:.1} MiB (+ zones {zones_z_mib:.1} MiB)",
            topo0.arc_coords.len()
        );
        std::fs::write(&stamp_path, &stamp)?;
    }
    println!("wrote {}", out_dir.display());
    Ok(())
}

/// The density grid, loaded once on first demand. Density is optional for
/// the viewer, so a failed load warns once and yields `None` thereafter.
enum LazyDensity {
    Unprobed,
    Probed(Option<utz_build::density::DensityGrid>),
}

impl LazyDensity {
    fn get(&mut self) -> Option<&utz_build::density::DensityGrid> {
        if matches!(self, Self::Unprobed) {
            *self = Self::Probed(
                match utz_build::density::DensityGrid::load(&utz_build::cache_dir()) {
                    Ok(grid) => Some(grid),
                    Err(error) => {
                        eprintln!(
                            "warning: density grid unavailable ({error}); density features disabled"
                        );
                        None
                    }
                },
            );
        }
        match self {
            Self::Probed(grid) => grid.as_ref(),
            Self::Unprobed => unreachable!("probed just above"),
        }
    }
}

/// A stamped output is fresh when every output file exists and the recorded
/// stamp matches the wanted one exactly.
fn fresh(stamp_path: &Path, want: &str, outputs: &[&Path]) -> bool {
    outputs.iter().all(|path| path.exists())
        && std::fs::read_to_string(stamp_path).is_ok_and(|have| have == want)
}

/// Computes a content hash (std `SipHash`) that identifies the generating
/// binary in stamps, so a rebuilt `utz_build` invalidates every blob its
/// code could shape.
fn hash_file(path: &Path) -> String {
    use std::hash::Hasher as _;
    std::fs::read(path).map_or_else(
        |_| "unreadable".into(),
        |bytes| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            hasher.write(&bytes);
            format!("{:016x}", hasher.finish())
        },
    )
}

/// Computes a len+mtime fingerprint for cache files that are only
/// rewritten when their content actually changed (the download cache and
/// its sidecars), and returns `none` when the file is absent.
fn file_fp(path: &Path) -> String {
    std::fs::metadata(path).map_or_else(
        |_| "none".into(),
        |metadata| {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            format!("{}-{mtime}", metadata.len())
        },
    )
}

/// Fingerprints a cached zip from the conditional-GET validators (`ETag` /
/// `Last-Modified`) stored beside the file (a 304 revalidation leaves both
/// untouched), falling back to len+mtime when no validators were stored.
fn zip_fp(zip: &Path) -> String {
    let name = zip
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or_default();
    let meta = zip.with_file_name(format!("{name}.headers"));
    match std::fs::read_to_string(&meta) {
        Ok(headers) if !headers.trim().is_empty() => {
            headers.split_whitespace().collect::<Vec<_>>().join(" ")
        }
        _ => file_fp(zip),
    }
}

/// Builds the zone lattice for the coarse-prefilter dominance view: it
/// encodes a fine-ε asset and lets the *runtime* answer a 0.1° lattice,
/// so the browser shows exactly what the shipped grid+PIP would answer.
/// The format is little-endian: `"uTZz" | u32 w | u32 h | u32 n_zones
/// | per zone: u16 len + utf8 tzid | pad to 2 | u16 ids[w·h]`, where
/// 0xFFFF means no zone, row 0 is 90°N, col 0 is 180°W, and cell centers
/// are sampled.
fn zones_bin(features: &[utz_build::Feat], dataset: &str) -> utz_build::Result<Vec<u8>> {
    const STEP: f64 = 0.1;
    let params = Params {
        dataset: utz_build::dataset(dataset)?.code(),
        tzbb_release: "webdist",
        epsilon_m: 100.0,
        quant_bits: 24,
        grid_deg: 2.0,
        // the asset never leaves this process (encode -> from_vec -> drop),
        // so compressing it would only burn CPU
        codec: Codec::Uncompressed,
        simplify: encode::SimplifyAlgorithm::default(),
        density_weight_floor: None,
        geom: encode::GeomEncoding::default(),
    };
    let finder = utz::Finder::from_vec(encode::encode(features, &params)?)?;
    let mut names: Vec<&str> = features
        .iter()
        .filter_map(|feature| feature.tzid.as_deref())
        .collect();
    names.sort_unstable();
    names.dedup();
    let zone_index: std::collections::HashMap<&str, u16> = names
        .iter()
        .enumerate()
        .map(|(i, &name)| (name, u16::try_from(i).expect("zone count fits u16")))
        .collect();

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "360/0.1 and 180/0.1 are small exact integers"
    )]
    let (width, height) = ((360.0 / STEP) as usize, (180.0 / STEP) as usize);
    let mut out = Vec::with_capacity(16 + width * height * 2);
    out.extend_from_slice(b"uTZz");
    out.extend_from_slice(
        &u32::try_from(width)
            .expect("lattice width fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u32::try_from(height)
            .expect("lattice height fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u32::try_from(names.len())
            .expect("zone count fits u32")
            .to_le_bytes(),
    );
    for name in &names {
        out.extend_from_slice(
            &u16::try_from(name.len())
                .expect("tzid len fits u16")
                .to_le_bytes(),
        );
        out.extend_from_slice(name.as_bytes());
    }
    out.resize(out.len().next_multiple_of(2), 0);
    for row in 0..height {
        #[expect(
            clippy::cast_precision_loss,
            reason = "row < height = 180/STEP lattice rows; exact"
        )]
        let lat = 90.0 - (row as f64 + 0.5) * STEP;
        for col in 0..width {
            #[expect(
                clippy::cast_precision_loss,
                reason = "col < width = 360/STEP lattice cols; exact"
            )]
            let lon = -180.0 + (col as f64 + 0.5) * STEP;
            let id = finder
                .lookup(utz::Position { lon, lat })
                .expect("lattice point in range")
                .and_then(|tzid| zone_index.get(tzid).copied())
                .unwrap_or(0xFFFF);
            out.extend_from_slice(&id.to_le_bytes());
        }
    }
    Ok(out)
}

/// Zlib-deflates `data` to `path` (the browser side inflates with
/// `DecompressionStream('deflate')`).
fn write_z(path: &Path, data: &[u8]) -> utz_build::Result<usize> {
    let deflated = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    std::fs::write(path, &deflated)?;
    Ok(deflated.len())
}

/// Builds `utz_viz` (simplify + live asset encode + stats surface) for
/// wasm32-unknown-unknown and returns the cdylib bytes. The emit feature
/// is off: it pulls in `utz_build`, whose std-only deps don't target wasm.
fn build_wasm() -> utz_build::Result<Vec<u8>> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    // cdylib requested here rather than in the crate's crate-type — see the
    // package comment in utz_viz/Cargo.toml
    let status = std::process::Command::new("cargo")
        .args([
            "rustc",
            "-p",
            "utz_viz",
            "--no-default-features",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--crate-type",
            "cdylib",
        ])
        .current_dir(root)
        .status()?;
    ensure!(
        status.success(),
        Error::Msg("wasm build failed — try: rustup target add wasm32-unknown-unknown".into())
    );
    // a relative CARGO_TARGET_DIR is resolved against the spawned
    // cargo's cwd, which is the workspace root above
    let target = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| format!("{root}/target"));
    let target = if std::path::Path::new(&target).is_absolute() {
        target
    } else {
        format!("{root}/{target}")
    };
    Ok(std::fs::read(format!(
        "{target}/wasm32-unknown-unknown/release/utz_viz.wasm"
    ))?)
}
