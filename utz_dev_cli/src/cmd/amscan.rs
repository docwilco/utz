//! Antimeridian scan: is TZBB with-oceans already split at ±180°? Flags
//! any edge whose lon span exceeds 180° (a true crossing stored planar)
//! and any coordinate outside [-180, 180] / [-90, 90].
//!
//! ```text
//! utz_dev_cli amscan [datasets...]
//! ```

#[derive(clap::Args)]
pub struct Args {
    /// datasets to scan: [land-]now|1970|all
    #[arg(default_values_t = [String::from("now"), String::from("1970")])]
    ds: Vec<String>,
}

/// # Errors
/// Dataset load/parse failure.
pub fn run(args: Args) -> utz_build::Result<()> {
    let datasets = args.ds;
    for dataset in &datasets {
        let features = utz_build::load(dataset)?;
        let (mut wide, mut out_of_bounds, mut on180) = (0usize, 0usize, 0usize);
        let mut wide_tzids: Vec<String> = Vec::new();
        for feature in &features {
            for poly in &feature.polys {
                for ring in poly {
                    let ring_len = ring.len();
                    for i in 0..ring_len {
                        let (x0, y0) = ring[i];
                        let (x1, _) = ring[(i + 1) % ring_len];
                        if (x1 - x0).abs() > 180.0 {
                            wide += 1;
                            let tzid = feature.tzid.clone().unwrap_or_default();
                            println!("  wide edge in {tzid}: ({x0},{y0}) -> ({x1},{})  ring {} of {} verts",
                                ring[(i + 1) % ring_len].1, i, ring_len);
                            if !wide_tzids.contains(&tzid) {
                                wide_tzids.push(tzid);
                            }
                        }
                        if !(-180.0..=180.0).contains(&x0) || !(-90.0..=90.0).contains(&y0) {
                            out_of_bounds += 1;
                        }
                        #[expect(
                            clippy::float_cmp,
                            reason = "TZBB writes literal ±180.0; counting exact antimeridian vertices"
                        )]
                        if x0.abs() == 180.0 {
                            on180 += 1;
                        }
                    }
                }
            }
        }
        println!("{}: {} features", dataset.to_uppercase(), features.len());
        println!("  edges spanning >180° lon (true crossings): {wide}  {wide_tzids:?}");
        println!("  coords outside ±180/±90: {out_of_bounds}");
        println!("  verts exactly on ±180: {on180}   (split polygons touch the line)\n");
    }
    Ok(())
}
