//! Measures the accuracy of simplified topologies vs the raw (ε=0) arcs.
//!
//! Every simplifier in the menu keeps a *subset* of the original vertices, so
//! each output segment covers a contiguous run of raw vertices and the
//! misassigned region decomposes exactly into "pockets" between the raw
//! sub-chain and its shortcut (split where the chain crosses the shortcut
//! line; the decomposition itself is `utz_viz::misassign`, shared
//! with the viewer's simplify worker). Per config this reports:
//!   - the max deviation (m, same flat 111 320 m/deg convention as
//!     `epsilon_m`),
//!   - the misassigned area (km², the sum of |pocket|), and
//!   - the misassigned population (people: pocket area × GHS-POP density
//!     at the pocket; pockets are ≤ ε wide, far below the 4′ grid, so one
//!     sample per pocket is essentially exact).
//!
//! ```text
//! utz_dev_cli accuracy [ds] [epsilon_m] [w_min] [rdp|vw|ii]
//! ```

use utz_build::density::DensityGrid;
use utz_encode::topo::{self, Simplify, Topology};
use utz_simplify::DensityWeight;
use utz_viz::misassign;

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    epsilon_m: f64,
    /// The weighted-floor multiplier at max density.
    #[arg(default_value_t = 0.052)]
    w_min: f64,
    /// The simplification algorithm, one of rdp|vw|ii.
    #[arg(default_value = "rdp")]
    algorithm: String,
}

/// # Errors
/// The command fails on an unknown algorithm name or a dataset
/// load/parse or density-grid load failure.
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, epsilon_m, w_min, algorithm_key) =
        (args.ds, args.epsilon_m, args.w_min, args.algorithm);
    let simplify_algorithm =
        utz_common::SimplifyAlgorithm::from_name(&algorithm_key).ok_or_else(|| {
            utz_build::Error::Msg(format!(
                "unknown algorithm {algorithm_key:?}: use none|rdp|vw|ii"
            ))
        })?;
    let algorithm = |epsilon_deg: f64| -> Simplify {
        utz_encode::encode::to_simplify(simplify_algorithm, epsilon_deg)
    };

    let features = utz_build::load(&dataset)?;
    let grid = DensityGrid::load(&utz_build::cache_dir())?;
    let raw_topology = topo::build_topology(&features, 0.0);
    let model = DensityWeight::new(w_min);

    let epsilon_deg = epsilon_m / utz_common::METERS_PER_DEG;
    let configs: Vec<(String, Topology)> = vec![
        (
            format!("uniform ε{epsilon_m}"),
            topo::build_topology_algorithm(&features, algorithm(epsilon_deg)),
        ),
        (
            format!("uniform ε{}", epsilon_m / 2.0),
            topo::build_topology_algorithm(&features, algorithm(epsilon_deg / 2.0)),
        ),
        (
            format!("weighted ε{epsilon_m}×{w_min}"),
            topo::build_topology_weighted(&features, algorithm(epsilon_deg), &|start, end| {
                model.weight(grid.max_along(start, end))
            }),
        ),
    ];

    println!("{dataset} · {algorithm_key} · misassignment vs raw ε=0 arcs\n");
    println!(
        "{:>22} {:>9} {:>10} {:>12} {:>14}",
        "config", "verts", "max dev", "misassigned", "misassigned"
    );
    println!(
        "{:>22} {:>9} {:>10} {:>12} {:>14}",
        "", "", "(m)", "area (km²)", "pop (people)"
    );
    for (name, topology) in &configs {
        let verts: usize = topology.arc_coords.iter().map(std::vec::Vec::len).sum();
        let measured = measure(&raw_topology, topology, &grid);
        println!(
            "{name:>22} {verts:>9} {:>10.1} {:>12.1} {:>14.0}",
            measured.max_dev_deg * utz_common::METERS_PER_DEG,
            measured.area_km2,
            measured.people
        );
    }
    Ok(())
}

#[derive(Default)]
struct Acc {
    max_dev_deg: f64,
    area_km2: f64,
    people: f64,
}

fn measure(raw_topology: &Topology, simplified_topology: &Topology, grid: &DensityGrid) -> Acc {
    let mut acc = Acc::default();
    // arcs are cut before simplification, so the two topologies' arc lists
    // correspond 1:1 by index
    for (original, simplified) in raw_topology
        .arc_coords
        .iter()
        .zip(&simplified_topology.arc_coords)
    {
        // simplified vertices are bit-exact copies of raw ones — walk-match
        // them back to raw indices
        let mut raw_indices = Vec::with_capacity(simplified.len());
        let mut j = 0;
        for &point in simplified {
            while original[j] != point {
                j += 1;
            }
            raw_indices.push(j);
            j += 1;
        }
        for window in raw_indices.windows(2) {
            let chain = &original[window[0]..=window[1]];
            acc.max_dev_deg = acc.max_dev_deg.max(max_dev_deg(chain));
            // shared pocket decomposition; people priced here with one
            // GHS-POP grid sample at the pocket's chain centroid (the
            // viewer instead averages its shipped per-vertex densities)
            misassign::pocket_scan(chain, None, |pocket| {
                let (lonc, latc) = (pocket.lon_sum / pocket.count, pocket.lat_sum / pocket.count);
                let km2 = pocket.area.abs()
                    * utz_common::KM_PER_DEG
                    * utz_common::KM_PER_DEG
                    * latc.to_radians().cos();
                acc.area_km2 += km2;
                acc.people += km2 * grid.sample(lonc, latc);
            });
        }
    }
    acc
}

/// Max perpendicular deviation of the chain's interior vertices from the
/// clamped shortcut chord (`chain.first()` → `chain.last()`), in flat
/// degrees (same convention as `epsilon_m`).
fn max_dev_deg(chain: &[(f64, f64)]) -> f64 {
    let (start, end) = (chain[0], *chain.last().unwrap());
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let len2 = dx * dx + dy * dy;
    let mut max_dev = 0.0f64;
    for &current in &chain[1..chain.len() - 1] {
        let dist2 = if len2 == 0.0 {
            (current.0 - start.0).powi(2) + (current.1 - start.1).powi(2)
        } else {
            let frac =
                (((current.0 - start.0) * dx + (current.1 - start.1) * dy) / len2).clamp(0.0, 1.0);
            (current.0 - start.0 - frac * dx).powi(2) + (current.1 - start.1 - frac * dy).powi(2)
        };
        max_dev = max_dev.max(dist2.sqrt());
    }
    max_dev
}
