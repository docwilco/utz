// Measurement backlog #7 (PLAN.md §15): antimeridian scan — is TZBB with-oceans
// already split at ±180°? Flags any edge whose lon span exceeds 180° (a true
// crossing stored planar) and any coordinate outside [-180, 180] / [-90, 90].
//
// usage: utz-build amscan [datasets...]

#[derive(clap::Args)]
pub struct Args {
    /// datasets to scan: [land-]now|1970|all
    #[arg(default_values_t = [String::from("now"), String::from("1970")])]
    ds: Vec<String>,
}

pub fn run(a: Args) -> utz_build::Result<()> {
    let dss = a.ds;
    for ds in &dss {
        let feats = utz_build::load(ds)?;
        let (mut wide, mut oob, mut on180) = (0usize, 0usize, 0usize);
        let mut wide_tzs: Vec<String> = Vec::new();
        for f in &feats {
            for p in &f.polys {
                for ring in p {
                    let n = ring.len();
                    for i in 0..n {
                        let (x0, y0) = ring[i];
                        let (x1, _) = ring[(i + 1) % n];
                        if (x1 - x0).abs() > 180.0 {
                            wide += 1;
                            let tz = f.tzid.clone().unwrap_or_default();
                            println!("  wide edge in {tz}: ({x0},{y0}) -> ({x1},{})  ring {} of {} verts",
                                ring[(i + 1) % n].1, i, n);
                            if !wide_tzs.contains(&tz) { wide_tzs.push(tz); }
                        }
                        if !(-180.0..=180.0).contains(&x0) || !(-90.0..=90.0).contains(&y0) { oob += 1; }
                        if x0.abs() == 180.0 { on180 += 1; }
                    }
                }
            }
        }
        println!("{}: {} features", ds.to_uppercase(), feats.len());
        println!("  edges spanning >180° lon (true crossings): {wide}  {wide_tzs:?}");
        println!("  coords outside ±180/±90: {oob}");
        println!("  verts exactly on ±180: {on180}   (split polygons touch the line)\n");
    }
    Ok(())
}
