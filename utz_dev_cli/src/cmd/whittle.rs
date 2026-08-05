//! Measures the per-stage size reduction of the whittling-down pipeline
//! on the real preset recipes: source `GeoJSON` → parsed f64 coordinates
//! → shared-arc topology → density-weighted simplification → quantized
//! coordinates → varint-coded arcs → serialized sections → compressed
//! asset. The stages mirror the utz
//! crate docs' "How it works" list; this is the command that keeps those
//! numbers honest. `--extended` adds the geometry-encodings matrix (raw
//! payload, recipe-codec asset, and xz asset per `GeomEncoding`):
//! the size columns of the ladder table on `GeomEncoding`.
//!
//! ```text
//! utz_dev_cli whittle [--extended] [tiny|compact|balanced|accurate|all]
//! ```

use utz_build::density::DensityGrid;
use utz_build::presets::{self, Recipe};
use utz_build::{download, loader};
use utz_encode::encode::{self, Codec, GeomEncoding, Params, SimplifyAlgorithm};
use utz_encode::{topo, Feat};
use utz_simplify::DensityWeight;
use utz_viz::{arc_verts, coord_count};

#[derive(clap::Args)]
pub struct Args {
    /// The preset recipe, one of tiny|compact|balanced|accurate|all
    /// (tiny-static is tiny with codec none and is reported with tiny).
    #[arg(default_value = "all")]
    preset: String,
    /// Also measures every geometry encoding per preset.
    #[arg(long)]
    extended: bool,
}

/// The uncompressed sibling of a recipe (e.g. `tiny-static` for `tiny`),
/// if the preset table carries one; it is reported as an extra stage of
/// its base preset instead of as a row of its own.
fn static_twin(recipe: &Recipe) -> Option<&'static Recipe> {
    presets::by_name(&format!("{}-static", recipe.name))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts ≪ 2^53; human-unit display"
)]
fn human(bytes: u64) -> String {
    let value = bytes as f64;
    if value >= 1024.0 * 1024.0 {
        format!("{:.1} MB", value / 1024.0 / 1024.0)
    } else {
        format!("{:.1} KB", value / 1024.0)
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "byte counts ≪ 2^53; ratio display"
)]
fn stage(label: &str, bytes: u64, previous: u64, source: u64) {
    println!(
        "  {label:<34} {:>10}   ÷{:<6.2} ÷{:<8.1}",
        human(bytes),
        previous as f64 / bytes as f64,
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
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("json") || extension.eq_ignore_ascii_case("geojson")
            })
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
fn ratio(numerator: u64, denominator: u64) -> f64 {
    numerator as f64 / denominator as f64
}

/// Prints the raw payload and compressed asset per geometry encoding,
/// with ratios vs `VarintArcs`: the size columns of the `GeomEncoding`
/// ladder table.
fn encodings_matrix(
    recipe: &Recipe,
    topology: &topo::Topology,
    features: &[Feat],
    release: &str,
) -> utz_build::Result<()> {
    println!(
        "  {:<18} {:>10} {:<7} {:>10} {:<7} {:>10} {:<7}",
        "encoding",
        "payload",
        "×va",
        format!("{:?}", recipe.codec),
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
        let params = Params {
            dataset: recipe.dataset,
            tzbb_release: release,
            epsilon_m: recipe.epsilon_m,
            quant_bits: recipe.quant_bits.bits(),
            grid_deg: recipe.grid_deg,
            codec: Codec::Uncompressed,
            simplify: recipe.simplify_algorithm,
            geom,
            density_weight_floor: recipe.density_weight_floor(),
        };
        let (payload, _) =
            encode::payload_from_topology(topology, &topology.arc_coords, features, &params)?;
        let container = encode::finish(&payload, recipe.codec)?;
        let xz = encode::finish(&payload, Codec::Xz)?;
        let sizes = (
            payload.len() as u64,
            container.len() as u64,
            xz.len() as u64,
        );
        let baseline = *base.get_or_insert(sizes);
        println!(
            "  {label:<18} {:>10} ×{:<6.2} {:>10} ×{:<6.2} {:>10} ×{:<6.2}",
            human(sizes.0),
            ratio(sizes.0, baseline.0),
            human(sizes.1),
            ratio(sizes.1, baseline.1),
            human(sizes.2),
            ratio(sizes.2, baseline.2),
        );
    }
    Ok(())
}

/// # Errors
/// The command fails on a density-grid load, dataset download/parse, or
/// encode failure.
pub fn run(args: &Args) -> utz_build::Result<()> {
    let cache = utz_build::cache_dir();
    let density = DensityGrid::load(&cache)?;
    let release = loader::resolve_release(&cache)?;

    for recipe in &presets::ALL {
        // -static twins ride along with their base preset's report
        if recipe.name.ends_with("-static") {
            continue;
        }
        if args.preset != "all" && args.preset != recipe.name {
            continue;
        }
        report_recipe(recipe, &cache, &density, &release, args.extended)?;
    }
    Ok(())
}

/// Measures and prints every whittling stage for one recipe.
fn report_recipe(
    recipe: &Recipe,
    cache: &std::path::Path,
    density: &DensityGrid,
    release: &str,
    extended: bool,
) -> utz_build::Result<()> {
    let zip_path = download::fetch(&loader::dataset_url(recipe.dataset, release), cache)?;
    let geojson = geojson_entry_size(&zip_path)?;
    let features = loader::load_geojson_zip(&zip_path)?;

    let floor_label = recipe
        .density_weight_floor()
        .map_or_else(|| "unweighted".to_string(), |floor| format!("w{floor}"));
    println!(
        "{} ({}, ε {} m {floor_label}, i{}, {:.4}°, {:?}, TZBB {release})",
        recipe.name,
        recipe.dataset.name(),
        recipe.epsilon_m,
        recipe.quant_bits.bits(),
        recipe.grid_deg,
        recipe.codec
    );
    println!(
        "  {:<34} {:>10}   {:<7} {:<8}",
        "stage", "size", "÷prev", "÷coords"
    );

    // parsed coordinates as f64 pairs: the in-memory baseline every
    // later stage is measured against
    let coords = coord_count(&features) * 16;
    println!(
        "  {:<34} {:>10}   {:<7} {:<8}",
        format!("source GeoJSON ({} zones)", features.len()),
        human(geojson),
        "",
        ""
    );
    stage("parsed coordinates (f64 pairs)", coords, geojson, coords);

    // shared-arc topology, no simplification: pure border dedup
    let raw_topology = topo::build_topology_algorithm(
        &features,
        encode::to_simplify(SimplifyAlgorithm::None, 0.0),
    );
    let arc_verts0: u64 = arc_verts(&raw_topology);
    stage(
        &format!(
            "shared-arc topology ({} arcs)",
            raw_topology.arc_coords.len()
        ),
        arc_verts0 * 16,
        coords,
        coords,
    );

    // the recipe's simplification, density-weighted when the recipe is
    let epsilon_deg = recipe.epsilon_m / 111_320.0;
    let algorithm = encode::to_simplify(recipe.simplify_algorithm, epsilon_deg);
    let topology = match recipe.density_weight_floor() {
        Some(floor) => {
            let weight = DensityWeight::new(floor);
            topo::build_topology_weighted(&features, algorithm, &|start, end| {
                weight.weight(density.max_along(start, end))
            })
        }
        None => topo::build_topology_algorithm(&features, algorithm),
    };
    let arc_verts1: u64 = arc_verts(&topology);
    stage(
        &format!("simplified (ε {} m {floor_label})", recipe.epsilon_m),
        arc_verts1 * 16,
        arc_verts0 * 16,
        coords,
    );

    // quantize + delta/varint code + grid + serialize
    let params = Params {
        dataset: recipe.dataset,
        tzbb_release: release,
        epsilon_m: recipe.epsilon_m,
        quant_bits: recipe.quant_bits.bits(),
        grid_deg: recipe.grid_deg,
        codec: Codec::Uncompressed,
        simplify: recipe.simplify_algorithm,
        geom: recipe.geom,
        density_weight_floor: recipe.density_weight_floor(),
    };
    let payload = report_payload(&topology, &features, &params, coords, arc_verts1)?;

    let container = encode::finish(&payload, recipe.codec)?;
    stage(
        &format!("compressed container ({:?})", recipe.codec),
        container.len() as u64,
        payload.len() as u64,
        coords,
    );
    if let Some(twin) = static_twin(recipe) {
        let flat = encode::finish(&payload, twin.codec)?;
        stage(
            &format!("uncompressed twin ({})", twin.name),
            flat.len() as u64,
            coords,
            coords,
        );
    }

    if extended {
        encodings_matrix(recipe, &topology, &features, release)?;
    }
    println!();
    Ok(())
}

/// Serializes the varint payload and prints the quantize / varint-code /
/// serialize stages; returns the payload for the compression stages.
fn report_payload(
    topology: &topo::Topology,
    features: &[Feat],
    params: &Params,
    coords: u64,
    arc_verts1: u64,
) -> utz_build::Result<Vec<u8>> {
    let (payload, stats) =
        encode::payload_from_topology(topology, &topology.arc_coords, features, params)?;
    // quantization alone: the surviving vertices at the declared
    // coordinate width (what fixed-width-arcs would store)
    let quantized = u64::from(stats.n_verts) * 2 * u64::from(params.quant_bits / 8);
    stage(
        &format!("quantized (i{} fixed-width)", params.quant_bits),
        quantized,
        arc_verts1 * 16,
        coords,
    );
    stage(
        "varint-coded arcs",
        u64::from(stats.arcs),
        quantized,
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
    Ok(payload)
}
