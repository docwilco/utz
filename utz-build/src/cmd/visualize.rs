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
use utz_build::{ensure, Error};

const DATASETS: [&str; 6] = ["now", "1970", "all", "land-now", "land-1970", "land-all"];

#[derive(clap::Args)]
pub struct Args {
    /// output directory for the viewer site
    #[arg(default_value = "webdist")]
    out: PathBuf,
    /// regenerate every blob even if its inputs are unchanged
    #[arg(long)]
    force: bool,
}

pub fn run(a: Args) -> utz_build::Result<()> {
    let out = a.out;
    std::fs::create_dir_all(&out)?;

    let wasm = build_wasm()?;
    std::fs::write(out.join("utz_encode.wasm"), &wasm)?;
    std::fs::write(out.join("index.html"), viz::webdist_index()?)?;
    #[expect(clippy::cast_precision_loss, reason = "wasm blob size ≪ 2^53; KiB display")]
    let wasm_kib = wasm.len() as f64 / 1024.0;
    println!("index.html + utz_encode.wasm ({wasm_kib:.1} KiB)");

    // Blob cache: each output carries a `.stamp-*` of the inputs that shaped
    // it — this binary, the TZBB zip (whose conditional-GET validators only
    // change when the content does), and the density sidecar. Matching stamp
    // + present outputs → skip the GeoJSON parse / lattice sampling /
    // deflate entirely.
    let cache = utz_build::cache_dir();
    let exe_fp = std::env::current_exe()
        .map_or_else(|_| "unknown".into(), |p| hash_file(&p));
    let release = utz_build::loader::resolve_release(&cache)?;
    let dens_fp = file_fp(&utz_build::density::sidecar_path(&cache));

    // density loads lazily: an all-fresh run touches neither the ~58 MB
    // sidecar nor the ~460 MB GHS-POP download behind its first build
    let mut dens = LazyDensity::Unprobed;

    let heat_path = out.join("heat.bin.z");
    let heat_stamp = format!("v1\nexe:{exe_fp}\ndens:{dens_fp}\n");
    let heat_stamp_path = out.join(".stamp-heat");
    if !a.force && fresh(&heat_stamp_path, &heat_stamp, &[&heat_path]) {
        println!("heat.bin.z: cached (inputs unchanged)");
    } else if let Some(g) = dens.get() {
        let n = write_z(&heat_path, &viz::heat_bin(g))?;
        #[expect(clippy::cast_precision_loss, reason = "deflated heatmap size ≪ 2^53; KiB display")]
        let heat_kib = n as f64 / 1024.0;
        println!("heat.bin.z: {heat_kib:.1} KiB");
        std::fs::write(&heat_stamp_path, &heat_stamp)?;
    }

    for ds in DATASETS {
        let d = utz_build::dataset(ds)?;
        let zip = utz_build::download::fetch(&utz_build::loader::dataset_url(d, &release), &cache)?;
        let stamp = format!("v1\nexe:{exe_fp}\nrelease:{release}\nzip:{}\ndens:{dens_fp}\n", zip_fp(&zip));
        let stamp_path = out.join(format!(".stamp-{ds}"));
        let bin_path = out.join(format!("{ds}.bin.z"));
        let zones_path = out.join(format!("zones-{ds}.bin.z"));
        if !a.force && fresh(&stamp_path, &stamp, &[&bin_path, &zones_path]) {
            println!("{ds}: cached (inputs unchanged)");
            continue;
        }
        let feats = utz_build::loader::load_geojson_zip(&zip)?;
        let topo0 = topo::build_topology(&feats, 0.0);
        let verts: usize = topo0.arc_coords.iter().map(std::vec::Vec::len).sum();
        let bin = viz::dataset_bin(&topo0, &feats, d.code(), "webdist", dens.get());
        let bin_z = write_z(&bin_path, &bin)?;
        let zones_z = write_z(&zones_path, &zones_bin(&feats, ds)?)?;
        #[expect(clippy::cast_precision_loss, reason = "raw/deflated bin sizes ≪ 2^53; MiB display")]
        let (raw_mib, bin_z_mib, zones_z_mib) = (
            bin.len() as f64 / f64::from(1 << 20),
            bin_z as f64 / f64::from(1 << 20),
            zones_z as f64 / f64::from(1 << 20),
        );
        println!(
            "{ds}: {} arcs, {verts} verts, {raw_mib:.1} MiB -> {bin_z_mib:.1} MiB (+ zones {zones_z_mib:.1} MiB)",
            topo0.arc_coords.len()
        );
        std::fs::write(&stamp_path, &stamp)?;
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// Density grid loaded once, on first demand — density is optional for the
/// viewer, so a failed load warns once and yields `None` thereafter.
enum LazyDensity {
    Unprobed,
    Probed(Option<utz_build::density::DensityGrid>),
}

impl LazyDensity {
    fn get(&mut self) -> Option<&utz_build::density::DensityGrid> {
        if matches!(self, Self::Unprobed) {
            *self = Self::Probed(match utz_build::density::DensityGrid::load(&utz_build::cache_dir()) {
                Ok(g) => Some(g),
                Err(e) => {
                    eprintln!("warning: density grid unavailable ({e}); density features disabled");
                    None
                }
            });
        }
        match self {
            Self::Probed(g) => g.as_ref(),
            Self::Unprobed => unreachable!("probed just above"),
        }
    }
}

/// A stamped output is fresh when every output file exists and the recorded
/// stamp matches the wanted one exactly.
fn fresh(stamp_path: &Path, want: &str, outputs: &[&Path]) -> bool {
    outputs.iter().all(|p| p.exists())
        && std::fs::read_to_string(stamp_path).is_ok_and(|have| have == want)
}

/// Content hash (std `SipHash`) — identifies the generating binary in stamps,
/// so a rebuilt utz-build invalidates every blob its code could shape.
fn hash_file(path: &Path) -> String {
    use std::hash::Hasher as _;
    std::fs::read(path).map_or_else(|_| "unreadable".into(), |b| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write(&b);
        format!("{:016x}", h.finish())
    })
}

/// len+mtime fingerprint for cache files that are only rewritten when their
/// content actually changed (the download cache and its sidecars); `none`
/// when absent.
fn file_fp(path: &Path) -> String {
    std::fs::metadata(path).map_or_else(|_| "none".into(), |m| {
        let mtime = m.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!("{}-{mtime}", m.len())
    })
}

/// Cached-zip fingerprint: the conditional-GET validators (`ETag` /
/// `Last-Modified`) stored beside the file — a 304 revalidation leaves both
/// untouched — falling back to len+mtime when no validators were stored.
fn zip_fp(zip: &Path) -> String {
    let name = zip.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let meta = zip.with_file_name(format!("{name}.headers"));
    match std::fs::read_to_string(&meta) {
        Ok(h) if !h.trim().is_empty() => h.split_whitespace().collect::<Vec<_>>().join(" "),
        _ => file_fp(zip),
    }
}

/// Zone lattice for the coarse-prefilter dominance view: encode a fine-ε
/// container and let the *runtime* answer a 0.1° lattice, so the browser
/// shows exactly what the shipped grid+PIP would answer. Format
/// (little-endian): `"uTZz" | u32 w | u32 h | u32 n_zones
/// | per zone: u16 len + utf8 tzid | pad to 2 | u16 ids[w·h]`
/// (0xFFFF = no zone; row 0 = 90°N, col 0 = 180°W, cell centers sampled).
fn zones_bin(feats: &[utz_build::Feat], ds: &str) -> utz_build::Result<Vec<u8>> {
    const STEP: f64 = 0.1;
    let p = Params {
        dataset: utz_build::dataset(ds)?.code(),
        tzbb_release: "webdist",
        eps_m: 100.0,
        quant_bits: 24,
        grid_deg: 2.0,
        codec: Codec::Zstd,
        simplify: encode::SimplifyAlgo::default(),
        geom: encode::GeomEncoding::default(),
    };
    let finder = utz::Finder::from_vec(encode::encode(feats, &p)?)
?;
    let mut names: Vec<&str> = feats.iter().filter_map(|f| f.tzid.as_deref()).collect();
    names.sort_unstable();
    names.dedup();
    let idx: std::collections::HashMap<&str, u16> =
        names.iter().enumerate().map(|(i, &n)| (n, u16::try_from(i).expect("zone count fits u16"))).collect();

    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "360/0.1 and 180/0.1 are small exact integers")]
    let (w, h) = ((360.0 / STEP) as usize, (180.0 / STEP) as usize);
    let mut o = Vec::with_capacity(16 + w * h * 2);
    o.extend_from_slice(b"uTZz");
    o.extend_from_slice(&u32::try_from(w).expect("lattice width fits u32").to_le_bytes());
    o.extend_from_slice(&u32::try_from(h).expect("lattice height fits u32").to_le_bytes());
    o.extend_from_slice(&u32::try_from(names.len()).expect("zone count fits u32").to_le_bytes());
    for n in &names {
        o.extend_from_slice(&u16::try_from(n.len()).expect("tzid len fits u16").to_le_bytes());
        o.extend_from_slice(n.as_bytes());
    }
    o.resize(o.len().next_multiple_of(2), 0);
    for r in 0..h {
        #[expect(clippy::cast_precision_loss, reason = "r < h = 180/STEP lattice rows; exact")]
        let lat = 90.0 - (r as f64 + 0.5) * STEP;
        for c in 0..w {
            #[expect(clippy::cast_precision_loss, reason = "c < w = 360/STEP lattice cols; exact")]
            let lon = -180.0 + (c as f64 + 0.5) * STEP;
            let id = finder.lookup(utz::Position { lon, lat }).and_then(|t| idx.get(t).copied()).unwrap_or(0xFFFF);
            o.extend_from_slice(&id.to_le_bytes());
        }
    }
    Ok(o)
}

/// zlib-deflate (the browser side inflates with `DecompressionStream('deflate')`).
fn write_z(path: &Path, data: &[u8]) -> utz_build::Result<usize> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    std::fs::write(path, &z)?;
    Ok(z.len())
}

/// Build utz-encode (simplify + live container encode) for
/// wasm32-unknown-unknown and return the cdylib bytes.
fn build_wasm() -> utz_build::Result<Vec<u8>> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
    // cdylib requested here rather than in utz-encode's crate-type — see the
    // [lib] comment in utz-encode/Cargo.toml
    let status = std::process::Command::new("cargo")
        .args(["rustc", "-p", "utz-encode", "--release", "--target", "wasm32-unknown-unknown", "--crate-type", "cdylib"])
        .current_dir(root)
        .status()?;
    ensure!(
        status.success(),
        Error::Msg("wasm build failed — try: rustup target add wasm32-unknown-unknown".into())
    );
    Ok(std::fs::read(format!("{root}/target/wasm32-unknown-unknown/release/utz_encode.wasm"))?)
}
