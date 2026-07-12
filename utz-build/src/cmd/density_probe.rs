//! Acceptance test for the GHS-POP ingest: first run downloads the ~460 MB
//! zip and builds the density sidecar; prints spot checks that fail loudly if
//! the tiff decode or geotransform is off.
//!
//!     utz-build density-probe

use utz_build::density::DensityGrid;
use utz_build::{ensure, Error};

#[derive(clap::Args)]
pub struct Args {}

pub fn run(_a: Args) -> utz_build::Result<()> {
    let t = std::time::Instant::now();
    let g = DensityGrid::load(&utz_build::cache_dir())?;
    println!("loaded {}x{} grid in {:.1?}", g.width, g.height, t.elapsed());
    #[expect(clippy::cast_precision_loss, reason = "grid dims w,h ≤ 43200 raster cells; exact")]
    let (lon1, lat1) = (g.lon0 + g.width as f64 * g.dlon, g.lat0 - g.height as f64 * g.dlat);
    println!(
        "extent: lon [{:.3}, {:.3}] lat [{:.3}, {:.3}] cell {:.4}x{:.4} deg",
        g.lon0,
        lon1,
        lat1,
        g.lat0,
        g.dlon,
        g.dlat
    );
    let (min, max, nz) = g.cells.iter().fold((f32::INFINITY, 0f32, 0usize), |(mn, mx, nz), &c| {
        (mn.min(c), mx.max(c), nz + usize::from(c > 0.0))
    });
    #[expect(clippy::cast_precision_loss, reason = "nz ≤ cell count ≪ 2^53; percentage display")]
    let pop_pct = 100.0 * nz as f64 / g.cells.len() as f64;
    println!("density min {min:.2} max {max:.0} p/km2, {pop_pct:.1}% cells populated");

    let probes = [
        ("central London", -0.1276, 51.5072, 3000.0, f64::INFINITY),
        ("Manhattan", -73.9712, 40.7831, 5000.0, f64::INFINITY),
        ("rural Kansas", -100.5, 38.9, 0.0, 100.0),
        ("mid-Atlantic", -30.0, 30.0, 0.0, 0.001),
        ("Sahara (Tanezrouft)", 0.5, 24.0, 0.0, 1.0),
        ("Tokyo", 139.767, 35.681, 3000.0, f64::INFINITY),
    ];
    let mut ok = true;
    for (name, lon, lat, lo, hi) in probes {
        let d = g.sample(lon, lat);
        let pass = (lo..=hi).contains(&d);
        ok &= pass;
        println!("{} {name}: {d:.1} p/km2 (expect {lo}..{hi})", if pass { "ok  " } else { "FAIL" });
    }
    // the edge-sampling path: a segment across the Channel from rural France
    // to rural England passes... nothing dense; London-crossing one does
    let along = g.max_along((-1.5, 50.6), (0.9, 52.2)); // Solent → Suffolk, through London
    println!("max_along Solent->Suffolk (crosses London): {along:.0} p/km2");
    ok &= along > 3000.0;

    ensure!(ok, Error::Msg("probe expectations failed".into()));
    println!("all probes pass");
    Ok(())
}
