//! Acceptance test for the GHS-POP ingest: first run downloads the ~460 MB
//! zip and builds the density sidecar; prints spot checks that fail loudly if
//! the tiff decode or geotransform is off.
//!
//! ```text
//! utz_dev_cli density-probe
//! ```

use utz_build::density::DensityGrid;
use utz_build::{ensure, Error};

#[derive(clap::Args)]
pub struct Args {}

/// # Errors
/// GHS-POP download or density-sidecar build/load failure, or a spot-check
/// probe landing outside its expected density range.
pub fn run(_args: Args) -> utz_build::Result<()> {
    let timer = std::time::Instant::now();
    let grid = DensityGrid::load(&utz_build::cache_dir())?;
    println!(
        "loaded {}x{} grid in {:.1?}",
        grid.width,
        grid.height,
        timer.elapsed()
    );
    #[expect(
        clippy::cast_precision_loss,
        reason = "grid dims width,height ≤ 43200 raster cells; exact"
    )]
    let (lon1, lat1) = (
        grid.lon0 + grid.width as f64 * grid.dlon,
        grid.lat0 - grid.height as f64 * grid.dlat,
    );
    println!(
        "extent: lon [{:.3}, {:.3}] lat [{:.3}, {:.3}] cell {:.4}x{:.4} deg",
        grid.lon0, lon1, lat1, grid.lat0, grid.dlon, grid.dlat
    );
    let (min, max, populated) = grid.cells.iter().fold(
        (f32::INFINITY, 0f32, 0usize),
        |(min, max, populated), &cell| {
            (
                min.min(cell),
                max.max(cell),
                populated + usize::from(cell > 0.0),
            )
        },
    );
    #[expect(
        clippy::cast_precision_loss,
        reason = "populated ≤ cell count ≪ 2^53; percentage display"
    )]
    let pop_pct = 100.0 * populated as f64 / grid.cells.len() as f64;
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
    for (name, lon, lat, low, high) in probes {
        let density = grid.sample(lon, lat);
        let pass = (low..=high).contains(&density);
        ok &= pass;
        println!(
            "{} {name}: {density:.1} p/km2 (expect {low}..{high})",
            if pass { "ok  " } else { "FAIL" }
        );
    }
    // the edge-sampling path: a segment across the Channel from rural France
    // to rural England passes... nothing dense; London-crossing one does
    let along = grid.max_along((-1.5, 50.6), (0.9, 52.2)); // Solent → Suffolk, through London
    println!("max_along Solent->Suffolk (crosses London): {along:.0} p/km2");
    ok &= along > 3000.0;

    ensure!(ok, Error::Msg("probe expectations failed".into()));
    println!("all probes pass");
    Ok(())
}
