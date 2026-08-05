//! Population density for density-weighted simplification ([GHS-POP
//! R2023A]).
//!
//! The source is the European Commission Joint Research Centre's Global
//! Human Settlement Layer population grid, © European Union 1995–2023
//! ([CC BY 4.0]; Schiavina, Freire, Carioli, MacManus 2023,
//! <https://doi.org/10.2905/2FF68A52-5B5B-4A22-8F40-C41DA8332CFE>).
//! It is a single global `GeoTIFF` in WGS84 at 30 arc-seconds (~1 km),
//! storing the population *count* per cell, as a free direct download.
//! The one-time build fetches the ~460 MB zip through the
//! [`crate::download`] cache, stream-decodes the tif tile by tile while
//! summing 8×8 blocks into a 4-arc-minute grid, converts counts to
//! people/km², and caches the result as a small flat sidecar (~58 MB).
//! Steady-state builds read only the sidecar.
//!
//! The resolution rationale is that weighting only needs
//! order-of-magnitude density near a boundary: 4′ (~7.4 km) cells are far
//! below any useful epsilon ceiling while keeping the grid cheap to hold in
//! memory.
//!
//! [GHS-POP R2023A]: https://human-settlement.emergency.copernicus.eu/ghs_pop2023.php
//! [CC BY 4.0]: https://creativecommons.org/licenses/by/4.0/

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use tiff::decoder::{Decoder, DecodingResult, Limits};

use crate::Error;
use tiff::tags::Tag;

/// The URL the GHS-POP population raster downloads from (JRC open data).
pub const GHS_POP_URL: &str = "https://jeodpp.jrc.ec.europa.eu/ftp/jrc-opendata/GHSL/\
GHS_POP_GLOBE_R2023A/GHS_POP_E2020_GLOBE_R2023A_4326_30ss/V1-0/\
GHS_POP_E2020_GLOBE_R2023A_4326_30ss_V1_0.zip";

/// 30″ source cells are summed in 8×8 blocks into the 4′ grid.
const DOWNSAMPLE: usize = 8;
/// The sidecar magic and version; bump it on a layout change.
const SIDECAR_MAGIC: &[u8; 4] = b"uTZd";
const SIDECAR_NAME: &str = "ghs_pop_e2020_4326_ds8.bin";

/// Returns the path of the decoded density sidecar inside `cache_dir`,
/// which lets callers fingerprint the density data without loading it
/// (the webdist blob cache).
#[must_use]
pub fn sidecar_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(SIDECAR_NAME)
}

/// Population density (people/km²) on a coarse global lon/lat grid.
/// Row 0 is the northernmost; `dlat` is positive.
pub struct DensityGrid {
    /// The grid's column count.
    pub width: usize,
    /// The grid's row count.
    pub height: usize,
    /// The west edge of cell (0,0).
    pub lon0: f64,
    /// The north edge of cell (0,0).
    pub lat0: f64,
    /// The cell width in degrees.
    pub dlon: f64,
    /// The cell height in degrees (positive; rows run north to south).
    pub dlat: f64,
    /// The row-major density samples in people/km².
    pub cells: Vec<f32>,
}

impl DensityGrid {
    /// Loads the grid from the sidecar cache, building it from GHS-POP on
    /// first use (downloading the zip via [`crate::download::fetch()`] if
    /// needed).
    ///
    /// # Errors
    /// Returns an error on a corrupt sidecar, or, on the first build, a
    /// GHS-POP download failure, a zip extraction or TIFF decode failure,
    /// or an I/O failure writing the sidecar.
    pub fn load(cache_dir: &Path) -> crate::Result<Self> {
        let sidecar = sidecar_path(cache_dir);
        if sidecar.exists() {
            return Self::read_sidecar(&sidecar);
        }
        let zip_path = crate::download::fetch(GHS_POP_URL, cache_dir)?;
        let tif_path = extract_tif(&zip_path, cache_dir)?;
        let grid = Self::from_ghs_pop_tif(&tif_path)?;
        grid.write_sidecar(&sidecar)?;
        // keep the zip for ETag revalidation; the extracted tif is redundant
        let _ = std::fs::remove_file(&tif_path);
        Ok(grid)
    }

    /// Returns the density at a point; outside the grid the density is 0.
    #[must_use]
    pub fn sample(&self, lon: f64, lat: f64) -> f64 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "floored cell index fits i64"
        )]
        let ix = ((lon - self.lon0) / self.dlon).floor() as i64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "floored cell index fits i64"
        )]
        let iy = ((self.lat0 - lat) / self.dlat).floor() as i64;
        self.cell_val(ix, iy)
    }

    /// Returns the maximum density over every grid cell the segment
    /// `a`–`b` crosses (an Amanatides–Woo traversal in cell space). This,
    /// not per-vertex sampling, is what boundary weighting uses: a long
    /// straight edge can cross a metro area without placing a vertex in
    /// it.
    #[must_use]
    pub fn max_along(&self, a: (f64, f64), b: (f64, f64)) -> f64 {
        // continuous cell-space coordinates (x → lon cells, y → rows south)
        let (x0, y0) = ((a.0 - self.lon0) / self.dlon, (self.lat0 - a.1) / self.dlat);
        let (x1, y1) = ((b.0 - self.lon0) / self.dlon, (self.lat0 - b.1) / self.dlat);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "floored cell coords fit i64"
        )]
        let (mut ix, mut iy) = (x0.floor() as i64, y0.floor() as i64);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "floored cell coords fit i64"
        )]
        let (ex, ey) = (x1.floor() as i64, y1.floor() as i64);
        let mut best = self.cell_val(ix, iy);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let (sx, sy) = (if dx > 0.0 { 1 } else { -1 }, if dy > 0.0 { 1 } else { -1 });
        // param t along the segment at the next x/y cell-boundary crossing
        let (mut tx, tdx) = if dx == 0.0 {
            (f64::INFINITY, f64::INFINITY)
        } else {
            #[expect(
                clippy::cast_precision_loss,
                reason = "|ix| ~ raster width ≤ 43200/8 for in-range lon; exact in f64"
            )]
            let first = (ix + i64::from(dx > 0.0)) as f64;
            (((first - x0) / dx).abs().max(0.0), (1.0 / dx).abs())
        };
        let (mut ty, tdy) = if dy == 0.0 {
            (f64::INFINITY, f64::INFINITY)
        } else {
            #[expect(
                clippy::cast_precision_loss,
                reason = "|iy| ~ raster height ≤ 21600/8 for in-range lat; exact in f64"
            )]
            let first = (iy + i64::from(dy > 0.0)) as f64;
            (((first - y0) / dy).abs().max(0.0), (1.0 / dy).abs())
        };
        // exactly one boundary crossing per step — no float termination games
        for _ in 0..(ex - ix).abs() + (ey - iy).abs() {
            if tx < ty {
                ix += sx;
                tx += tdx;
            } else {
                iy += sy;
                ty += tdy;
            }
            best = best.max(self.cell_val(ix, iy));
        }
        best
    }

    #[expect(clippy::cast_possible_wrap, reason = "raster dims ≤ 43200 ≪ i64::MAX")]
    fn cell_val(&self, ix: i64, iy: i64) -> f64 {
        if iy < 0 || iy >= self.height as i64 {
            return 0.0;
        }
        // wrap longitude when the grid spans the full 360° (it does for
        // GHS-POP; the guard keeps synthetic test grids honest)
        #[expect(
            clippy::cast_precision_loss,
            reason = "raster width ≤ 43200/8 (GHS-POP downsampled); exact in f64"
        )]
        let ix = if (self.width as f64 * self.dlon - 360.0).abs() < 1e-6 {
            ix.rem_euclid(self.width as i64)
        } else if ix < 0 || ix >= self.width as i64 {
            return 0.0;
        } else {
            ix
        };
        let (ix, iy) = (
            usize::try_from(ix).expect("checked in range"),
            usize::try_from(iy).expect("checked in range"),
        );
        f64::from(self.cells[iy * self.width + ix])
    }

    /// Decodes the GHS-POP `GeoTIFF`, summing 8×8 pixel blocks and
    /// converting population counts to people/km².
    ///
    /// # Errors
    /// Returns an error on an I/O or TIFF decode failure, missing
    /// geotransform tags, or a sample format other than f32/f64.
    ///
    /// # Panics
    /// Panics if the source raster's dimensions or chunk count exceed u32
    /// (not reachable for GHS-POP).
    pub fn from_ghs_pop_tif(tif_path: &Path) -> crate::Result<Self> {
        use utz_common::KM_PER_DEG;
        let mut decoder = Decoder::new(BufReader::new(std::fs::File::open(tif_path)?))?
            .with_limits(Limits::unlimited());
        let (source_width, source_height) = decoder.dimensions()?;
        let (source_width, source_height) = (source_width as usize, source_height as usize);
        // geotransform: pixel scale + tiepoint (don't assume ±180/±90 cover)
        let scale = decoder.get_tag_f64_vec(Tag::ModelPixelScaleTag)?;
        let tiepoint = decoder.get_tag_f64_vec(Tag::ModelTiepointTag)?;
        crate::ensure!(
            scale.len() >= 2 && tiepoint.len() >= 5,
            Error::MissingGeotransform
        );
        let (source_dlon, source_dlat) = (scale[0], scale[1]);
        let (lon0, lat0) = (
            tiepoint[3] - tiepoint[0] * source_dlon,
            tiepoint[4] + tiepoint[1] * source_dlat,
        );

        let (width, height) = (
            source_width.div_ceil(DOWNSAMPLE),
            source_height.div_ceil(DOWNSAMPLE),
        );
        let mut sums = vec![0f64; width * height];
        let (chunk_width, chunk_height) = decoder.chunk_dimensions();
        let (chunk_width, chunk_height) = (chunk_width as usize, chunk_height as usize);
        let across = source_width.div_ceil(chunk_width);
        for chunk in 0..u32::try_from(across * source_height.div_ceil(chunk_height))
            .expect("chunk count fits u32")
        {
            let (x_offset, y_offset) = (
                (chunk as usize % across) * chunk_width,
                (chunk as usize / across) * chunk_height,
            );
            // GDAL writes all-nodata ocean tiles as sparse (offset 0) — skip
            let Ok(data) = decoder.read_chunk(chunk) else {
                continue;
            };
            let (data_width, data_height) = decoder.chunk_data_dimensions(chunk);
            let (data_width, data_height) = (data_width as usize, data_height as usize);
            let mut add = |px: usize, py: usize, value: f64| {
                // nodata is -200 → clamp negatives to zero population
                if value > 0.0 {
                    sums[(y_offset + py) / DOWNSAMPLE * width + (x_offset + px) / DOWNSAMPLE] +=
                        value;
                }
            };
            match data {
                DecodingResult::F32(samples) => {
                    for py in 0..data_height {
                        for px in 0..data_width {
                            add(px, py, f64::from(samples[py * data_width + px]));
                        }
                    }
                }
                DecodingResult::F64(samples) => {
                    for py in 0..data_height {
                        for px in 0..data_width {
                            add(px, py, samples[py * data_width + px]);
                        }
                    }
                }
                other => {
                    return Err(Error::BadSampleFormat {
                        format: format!("{other:?}"),
                    })
                }
            }
        }

        // counts → people/km². 111.32 km/deg with a cos(lat) lon correction
        // is plenty: weighting needs order-of-magnitude density, not
        // demographics. cos clamped at 85° (population there ≈ 0 anyway).
        #[expect(clippy::cast_precision_loss, reason = "DOWNSAMPLE = 8, exact in f64")]
        let (dlon, dlat) = (
            source_dlon * DOWNSAMPLE as f64,
            source_dlat * DOWNSAMPLE as f64,
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "density → f32 grid cell, rounding is fine"
        )]
        let cells = (0..width * height)
            .map(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "row index i/width < height ≤ 21600/8; exact in f64"
                )]
                let center_lat = lat0 - (((i / width) as f64) + 0.5) * dlat;
                let coslat = center_lat
                    .to_radians()
                    .cos()
                    .max((85f64).to_radians().cos());
                let area = (dlat * KM_PER_DEG) * (dlon * KM_PER_DEG * coslat);
                (sums[i] / area) as f32
            })
            .collect();
        Ok(Self {
            width,
            height,
            lon0,
            lat0,
            dlon,
            dlat,
            cells,
        })
    }

    fn write_sidecar(&self, path: &Path) -> crate::Result<()> {
        let partial_path = path.with_extension("part");
        let mut writer = BufWriter::new(std::fs::File::create(&partial_path)?);
        writer.write_all(SIDECAR_MAGIC)?;
        writer.write_all(
            &u32::try_from(self.width)
                .expect("grid width fits u32")
                .to_le_bytes(),
        )?;
        writer.write_all(
            &u32::try_from(self.height)
                .expect("grid height fits u32")
                .to_le_bytes(),
        )?;
        for value in [self.lon0, self.lat0, self.dlon, self.dlat] {
            writer.write_all(&value.to_le_bytes())?;
        }
        for cell in &self.cells {
            writer.write_all(&cell.to_le_bytes())?;
        }
        writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        std::fs::rename(&partial_path, path)?;
        Ok(())
    }

    fn read_sidecar(path: &Path) -> crate::Result<Self> {
        let mut reader = BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        crate::ensure!(&magic == SIDECAR_MAGIC, Error::BadSidecar("bad magic"));
        let mut word = [0u8; 4];
        reader.read_exact(&mut word)?;
        let width = u32::from_le_bytes(word) as usize;
        reader.read_exact(&mut word)?;
        let height = u32::from_le_bytes(word) as usize;
        let mut scalar = [0u8; 8];
        let mut geotransform = [0f64; 4];
        for field in &mut geotransform {
            reader.read_exact(&mut scalar)?;
            *field = f64::from_le_bytes(scalar);
        }
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        crate::ensure!(
            bytes.len() == width * height * 4,
            Error::BadSidecar("size mismatch")
        );
        let cells = bytes
            .chunks_exact(4)
            .map(|cell_bytes| f32::from_le_bytes(cell_bytes.try_into().unwrap()))
            .collect();
        Ok(Self {
            width,
            height,
            lon0: geotransform[0],
            lat0: geotransform[1],
            dlon: geotransform[2],
            dlat: geotransform[3],
            cells,
        })
    }
}

/// Extracts the single `.tif` entry from the GHS-POP zip next to it
/// (the tiff decoder needs `Seek`, which zip entries don't offer).
fn extract_tif(zip_path: &Path, cache_dir: &Path) -> crate::Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let name = archive
        .file_names()
        .find(|entry_name| {
            Path::new(entry_name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tif"))
        })
        .ok_or_else(|| Error::NoTif {
            zip: zip_path.into(),
        })?
        .to_string();
    let out = cache_dir.join(name.rsplit('/').next().unwrap());
    let mut entry = archive.by_name(&name)?;
    let partial_path = out.with_extension("part");
    std::io::copy(
        &mut entry,
        &mut BufWriter::new(std::fs::File::create(&partial_path)?),
    )?;
    std::fs::rename(&partial_path, &out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10×10 one-degree cells covering lon/lat [0,10]×[0,10], all zero except
    /// a hot cell at (5,5)..(6,6).
    fn grid() -> DensityGrid {
        let mut cells = vec![0f32; 100];
        cells[4 * 10 + 5] = 1000.0; // row 4 = lat 5..6 (row 0 is lat 9..10)
        DensityGrid {
            width: 10,
            height: 10,
            lon0: 0.0,
            lat0: 10.0,
            dlon: 1.0,
            dlat: 1.0,
            cells,
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "cell values stored exactly (0.0/1000.0); approximate equality would weaken the test"
    )]
    fn sample_hits_the_right_cell() {
        let grid = grid();
        assert_eq!(grid.sample(5.5, 5.5), 1000.0);
        assert_eq!(grid.sample(4.5, 5.5), 0.0);
        assert_eq!(grid.sample(5.5, 4.5), 0.0);
        assert_eq!(grid.sample(-20.0, 5.5), 0.0); // outside (grid isn't 360°-wide)
        assert_eq!(grid.sample(5.5, 20.0), 0.0);
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "cell values stored exactly (0.0/1000.0); approximate equality would weaken the test"
    )]
    fn max_along_sees_cells_between_vertices() {
        let grid = grid();
        // horizontal crossing: both endpoints in cold cells, hot in between
        assert_eq!(grid.max_along((1.5, 5.5), (8.5, 5.5)), 1000.0);
        // diagonal crossing
        assert_eq!(grid.max_along((4.2, 4.2), (6.8, 6.8)), 1000.0);
        // parallel misses
        assert_eq!(grid.max_along((1.5, 3.5), (8.5, 3.5)), 0.0);
        // degenerate (point) segment
        assert_eq!(grid.max_along((5.5, 5.5), (5.5, 5.5)), 1000.0);
        // vertical through the hot column
        assert_eq!(grid.max_along((5.5, 1.5), (5.5, 8.5)), 1000.0);
    }

    #[test]
    fn sidecar_roundtrip() {
        let grid = grid();
        let dir = std::env::temp_dir().join("utz_density_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SIDECAR_NAME);
        grid.write_sidecar(&path).unwrap();
        let restored = DensityGrid::read_sidecar(&path).unwrap();
        assert_eq!(
            (
                restored.width,
                restored.height,
                restored.lon0,
                restored.lat0,
                restored.dlon,
                restored.dlat
            ),
            (10, 10, 0.0, 10.0, 1.0, 1.0)
        );
        assert_eq!(restored.cells, grid.cells);
        std::fs::remove_file(&path).unwrap();
    }
}
