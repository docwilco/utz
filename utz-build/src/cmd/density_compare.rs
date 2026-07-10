//! Uniform vs population-weighted simplification at the same ε ceiling:
//! stored-vertex counts binned by local density, plus the container size
//! delta. The visual comparison lives in the live viewer (`visualize live`),
//! which has a per-set pop-weight toggle and the density heatmap.
//!
//!     utz-build density-compare [ds] [eps_m] [w_min]

use utz_build::density::DensityGrid;
use utz_build::encode::{self, Codec, Params};
use utz_build::topo;
use utz_simplify::DensityWeight;

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// weighted-floor multiplier at max density
    #[arg(default_value_t = 0.1)]
    w_min: f64,
}

pub fn run(a: Args) -> utz_build::Result<()> {
    // stored vertices binned by the density at the vertex itself
    const BANDS: [(f64, f64, &str); 4] = [
        (0.0, 5.0, "<5 (empty)"),
        (5.0, 100.0, "5-100 (rural)"),
        (100.0, 1000.0, "100-1k (town)"),
        (1000.0, f64::INFINITY, ">=1k (city)"),
    ];
    let (ds, eps_m, w_min) = (a.ds, a.eps_m, a.w_min);

    let feats = utz_build::load(&ds)?;
    let grid = DensityGrid::load(&utz_build::cache_dir())?;
    let model = DensityWeight::new(w_min);

    let eps_deg = eps_m / 111_320.0;
    let t_u = topo::build_topology(&feats, eps_deg);
    let t_w = topo::build_topology_weighted(&feats, topo::Simplify::Rdp { eps: eps_deg }, &|a, b| {
        model.weight(grid.max_along(a, b))
    });

    let hist = |t: &topo::Topology| -> [usize; 4] {
        let mut h = [0usize; 4];
        for a in &t.arc_coords {
            for &(x, y) in a {
                let d = grid.sample(x, y);
                h[BANDS.iter().position(|b| d >= b.0 && d < b.1).unwrap()] += 1;
            }
        }
        h
    };
    let (hu, hw) = (hist(&t_u), hist(&t_w));

    println!("{ds} · RDP ε {eps_m} m ceiling · weighted floor ×{w_min} (ε {} m)\n", eps_m * w_min);
    println!("{:>16} {:>10} {:>10} {:>9}", "density band", "uniform", "weighted", "delta");
    for (i, b) in BANDS.iter().enumerate() {
        println!("{:>16} {:>10} {:>10} {:>+9}", b.2, hu[i], hw[i], hw[i] as i64 - hu[i] as i64);
    }
    let (su, sw) = (hu.iter().sum::<usize>(), hw.iter().sum::<usize>());
    println!("{:>16} {su:>10} {sw:>10} {:>+9}  ({:+.1}%)\n", "total", sw as i64 - su as i64, 100.0 * (sw as f64 / su as f64 - 1.0));

    // container size delta (same knobs, zstd; topologies already built above)
    let p = Params {
        dataset: 0,
        tzbb_release: "density-compare",
        eps_m,
        quant_bits: 24,
        grid_deg: 2.0,
        codec: Codec::Zstd,
        simplify: encode::SimplifyAlgo::default(),
        geom: encode::GeomEncoding::default(),
    };
    let container = |t: &topo::Topology| -> utz_build::Result<Vec<u8>> {
        Ok(encode::finish(&encode::payload_from_topology(t, &t.arc_coords, &feats, &p)?.0, p.codec)?)
    };
    let cu = container(&t_u)?;
    let cw = container(&t_w)?;
    println!(
        "container (i24, zstd): uniform {:.1} KiB -> weighted {:.1} KiB ({:+.1}%)",
        cu.len() as f64 / 1024.0,
        cw.len() as f64 / 1024.0,
        100.0 * (cw.len() as f64 / cu.len() as f64 - 1.0)
    );
    Ok(())
}
