//! Per-stage size reduction of the whittling-down pipeline, measured on
//! the real preset recipes: source `GeoJSON` → parsed f64 coordinates →
//! shared-arc topology → density-weighted simplification → quantized +
//! serialized sections → compressed asset. The stages mirror the utz
//! crate docs' "How it works" list; this is the command that keeps those
//! numbers honest. `--extended` adds the geometry-encodings matrix (raw
//! payload, recipe-codec asset, and xz asset per `GeomEncoding`),
//! the size columns of the ladder table on `GeomEncoding`.
//!
//! ```text
//! utz-build-cli whittle [--extended] [tiny|compact|balanced|accurate|all]
//! ```

use utz_build::density::DensityGrid;
use utz_build::encode::{self, Codec, GeomEncoding, Params, SimplifyAlgo};
use utz_build::{download, loader, topo, Feat};
use utz_simplify::DensityWeight;
use utz_whittle_stats::{arc_verts, coord_count};

#[derive(clap::Args)]
pub struct Args {
    /// preset recipe: tiny|compact|balanced|accurate|all
    /// (tiny-static is tiny with codec none; reported with tiny)
    #[arg(default_value = "all")]
    preset: String,
    /// also measure every geometry encoding per preset
    #[arg(long)]
    extended: bool,
}

/// One preset recipe, mirroring `utz_build::Config::{tiny,compact,balanced,
/// accurate}` (pinned there and in scripts/gen-presets.sh).
struct Recipe {
    name: &'static str,
    ds: &'static str,
    eps_m: f64,
    quant_bits: u32,
    grid_deg: f64,
    w_min: f64,
    codec: Codec,
    /// also report the uncompressed asset (the `-static` twin)
    static_twin: bool,
}

const RECIPES: [Recipe; 4] = [
    Recipe {
        name: "tiny",
        ds: "now",
        eps_m: 10_000.0,
        quant_bits: 16,
        grid_deg: 2.0,
        w_min: 0.001,
        codec: Codec::Gzip,
        static_twin: true,
    },
    Recipe {
        name: "compact",
        ds: "now",
        eps_m: 1_000.0,
        quant_bits: 24,
        grid_deg: 4.0 / 3.0,
        w_min: 0.001,
        codec: Codec::Xz,
        static_twin: false,
    },
    Recipe {
        name: "balanced",
        ds: "now",
        eps_m: 50.0,
        quant_bits: 24,
        grid_deg: 2.0 / 3.0,
        w_min: 0.020,
        codec: Codec::Brotli,
        static_twin: false,
    },
    Recipe {
        name: "accurate",
        ds: "all",
        eps_m: 10.0,
        quant_bits: 32,
        grid_deg: 0.5,
        w_min: 0.10,
        codec: Codec::Brotli,
        static_twin: false,
    },
];

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts ≪ 2^53; human-unit display"
)]
fn human(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.1} KB", b / 1024.0)
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts ≪ 2^53; ratio display"
)]
fn stage(label: &str, bytes: u64, prev: u64, source: u64) {
    println!(
        "  {label:<34} {:>10}   ÷{:<6.2} ÷{:<8.1}",
        human(bytes),
        prev as f64 / bytes as f64,
        source as f64 / bytes as f64
    );
}

fn geojson_entry_size(zip_path: &std::path::Path) -> utz_build::Result<u64> {
    let file = std::fs::File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))?;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if std::path::Path::new(entry.name())
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("geojson"))
        {
            return Ok(entry.size());
        }
    }
    Ok(0)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts ≪ 2^53; ratio display"
)]
fn ratio(n: u64, d: u64) -> f64 {
    n as f64 / d as f64
}

/// Raw payload + compressed asset per geometry encoding, with ratios
/// vs `VarintArcs`: the size columns of the `GeomEncoding` ladder table.
fn encodings_matrix(
    r: &Recipe,
    t: &topo::Topology,
    feats: &[Feat],
    release: &str,
) -> utz_build::Result<()> {
    println!(
        "  {:<18} {:>10} {:<7} {:>10} {:<7} {:>10} {:<7}",
        "encoding",
        "payload",
        "×va",
        format!("{:?}", r.codec),
        "×va",
        "xz",
        "×va"
    );
    let mut base: Option<(u64, u64, u64)> = None;
    for (geom, label) in [
        (GeomEncoding::VarintArcs, "varint-arcs"),
        (GeomEncoding::FixedWidthArcs, "fixed-width-arcs"),
        (GeomEncoding::FullRings, "full-rings"),
        (GeomEncoding::Coarse, "coarse"),
    ] {
        let p = Params {
            dataset: utz_build::dataset(r.ds)?.code(),
            tzbb_release: release,
            eps_m: r.eps_m,
            quant_bits: r.quant_bits,
            grid_deg: r.grid_deg,
            codec: Codec::Uncompressed,
            simplify: SimplifyAlgo::Rdp,
            geom,
        };
        let (payload, _) = encode::payload_from_topology(t, &t.arc_coords, feats, &p)?;
        let container = encode::finish(&payload, r.codec)?;
        let xz = encode::finish(&payload, Codec::Xz)?;
        let sizes = (
            payload.len() as u64,
            container.len() as u64,
            xz.len() as u64,
        );
        let b = *base.get_or_insert(sizes);
        println!(
            "  {label:<18} {:>10} ×{:<6.2} {:>10} ×{:<6.2} {:>10} ×{:<6.2}",
            human(sizes.0),
            ratio(sizes.0, b.0),
            human(sizes.1),
            ratio(sizes.1, b.1),
            human(sizes.2),
            ratio(sizes.2, b.2),
        );
    }
    Ok(())
}

/// # Errors
/// Density-grid load, dataset download/parse, or encode failure.
pub fn run(a: &Args) -> utz_build::Result<()> {
    let cache = utz_build::cache_dir();
    let density = DensityGrid::load(&cache)?;
    let release = loader::resolve_release(&cache)?;

    for r in RECIPES {
        if a.preset != "all" && a.preset != r.name {
            continue;
        }
        let ds = utz_build::dataset(r.ds)?;
        let zip_path = download::fetch(&loader::dataset_url(ds, &release), &cache)?;
        let geojson = geojson_entry_size(&zip_path)?;
        let feats = loader::load_geojson_zip(&zip_path)?;

        println!(
            "{} ({}, ε {} m w{}, i{}, {:.4}°, {:?}, TZBB {release})",
            r.name, r.ds, r.eps_m, r.w_min, r.quant_bits, r.grid_deg, r.codec
        );
        println!(
            "  {:<34} {:>10}   {:<7} {:<8}",
            "stage", "size", "÷prev", "÷coords"
        );

        // parsed coordinates as f64 pairs: the in-memory baseline every
        // later stage is measured against
        let coords = coord_count(&feats) * 16;
        println!(
            "  {:<34} {:>10}   {:<7} {:<8}",
            format!("source GeoJSON ({} zones)", feats.len()),
            human(geojson),
            "",
            ""
        );
        stage("parsed coordinates (f64 pairs)", coords, geojson, coords);

        // shared-arc topology, no simplification: pure border dedup
        let t0 = topo::build_topology_algo(&feats, encode::to_simplify(SimplifyAlgo::None, 0.0));
        let arc_verts0: u64 = arc_verts(&t0);
        stage(
            &format!("shared-arc topology ({} arcs)", t0.arc_coords.len()),
            arc_verts0 * 16,
            coords,
            coords,
        );

        // the recipe's density-weighted simplification
        let eps_deg = r.eps_m / 111_320.0;
        let weight = DensityWeight::new(r.w_min);
        let algo = encode::to_simplify(SimplifyAlgo::Rdp, eps_deg);
        let t = topo::build_topology_weighted(&feats, algo, &|a, b| {
            weight.weight(density.max_along(a, b))
        });
        let arc_verts1: u64 = arc_verts(&t);
        stage(
            &format!("simplified (ε {} m weighted)", r.eps_m),
            arc_verts1 * 16,
            arc_verts0 * 16,
            coords,
        );

        // quantize + delta/varint code + grid + serialize
        let p = Params {
            dataset: ds.code(),
            tzbb_release: &release,
            eps_m: r.eps_m,
            quant_bits: r.quant_bits,
            grid_deg: r.grid_deg,
            codec: Codec::Uncompressed,
            simplify: SimplifyAlgo::Rdp,
            geom: encode::GeomEncoding::VarintArcs,
        };
        let (payload, stats) = encode::payload_from_topology(&t, &t.arc_coords, &feats, &p)?;
        stage(
            &format!("quantized + varint arcs (i{})", r.quant_bits),
            u64::from(stats.arcs),
            arc_verts1 * 16,
            coords,
        );
        println!(
            "  {:<34} {:>10}   (header {} + zones {} + rings {} + grid {})",
            format!("serialized payload ({} verts)", stats.n_verts),
            human(payload.len() as u64),
            human(u64::from(stats.header)),
            human(u64::from(stats.zones)),
            human(u64::from(stats.rings)),
            human(u64::from(stats.grid)),
        );

        let container = encode::finish(&payload, r.codec)?;
        stage(
            &format!("compressed container ({:?})", r.codec),
            container.len() as u64,
            payload.len() as u64,
            coords,
        );
        if r.static_twin {
            let flat = encode::finish(&payload, Codec::Uncompressed)?;
            stage(
                "uncompressed twin (-static)",
                flat.len() as u64,
                coords,
                coords,
            );
        }

        if a.extended {
            encodings_matrix(&r, &t, &feats, &release)?;
        }
        println!();
    }
    Ok(())
}
