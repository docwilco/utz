//! Population density for density-weighted simplification (GHS-POP R2023A).
//!
//! Source: JRC's Global Human Settlement Layer population grid — a single
//! global `GeoTIFF`, WGS84, 30 arc-seconds (~1 km), population *count* per
//! cell, free direct download. One-time: fetch the ~460 MB zip through the
//! [`crate::download`] cache, stream-decode the tif tile by tile summing 8×8
//! blocks into a 4-arc-minute grid, convert counts → people/km², and cache
//! the result as a small flat sidecar (~58 MB). Steady-state builds read only
//! the sidecar.
//!
//! Resolution rationale: weighting only needs order-of-magnitude density near
//! a boundary; 4′ (~7.4 km) cells are far below any useful eps ceiling while
//! keeping the grid in-memory cheap.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use tiff::decoder::{Decoder, DecodingResult, Limits};

use crate::Error;
use tiff::tags::Tag;

pub const GHS_POP_URL: &str = "https://jeodpp.jrc.ec.europa.eu/ftp/jrc-opendata/GHSL/\
GHS_POP_GLOBE_R2023A/GHS_POP_E2020_GLOBE_R2023A_4326_30ss/V1-0/\
GHS_POP_E2020_GLOBE_R2023A_4326_30ss_V1_0.zip";

/// 30″ source cells summed into 8×8 blocks → 4′ grid.
const DOWNSAMPLE: usize = 8;
/// Sidecar magic + version (bump on layout change).
const SIDECAR_MAGIC: &[u8; 4] = b"uTZd";
const SIDECAR_NAME: &str = "ghs_pop_e2020_4326_ds8.bin";

/// Path of the decoded density sidecar inside `cache_dir` — lets callers
/// fingerprint the density data without loading it (webdist blob cache).
#[must_use]
pub fn sidecar_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(SIDECAR_NAME)
}

/// Population density (people/km²) on a coarse global lon/lat grid.
/// Row 0 is the northernmost; `dlat` is positive.
pub struct DensityGrid {
    pub width: usize,
    pub height: usize,
    /// west edge of cell (0,0)
    pub lon0: f64,
    /// north edge of cell (0,0)
    pub lat0: f64,
    pub dlon: f64,
    pub dlat: f64,
    pub cells: Vec<f32>,
}

impl DensityGrid {
    /// Load from the sidecar cache, building it from GHS-POP on first use
    /// (downloads the zip via [`crate::download::fetch`] if needed).
    ///
    /// # Errors
    /// Corrupt sidecar, or on first build: GHS-POP download failure, zip
    /// extraction/TIFF decode failure, or I/O writing the sidecar.
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

    /// Density at a point; outside the grid → 0.
    #[must_use]
    pub fn sample(&self, lon: f64, lat: f64) -> f64 {
        #[expect(clippy::cast_possible_truncation, reason = "floored cell index fits i64")]
        let ix = ((lon - self.lon0) / self.dlon).floor() as i64;
        #[expect(clippy::cast_possible_truncation, reason = "floored cell index fits i64")]
        let iy = ((self.lat0 - lat) / self.dlat).floor() as i64;
        self.cell_val(ix, iy)
    }

    /// Max density over every grid cell the segment `a`–`b` crosses
    /// (Amanatides–Woo traversal in cell space). This — not per-vertex
    /// sampling — is what boundary weighting uses: a long straight edge can
    /// cross a metro area without placing a vertex in it.
    #[must_use]
    pub fn max_along(&self, a: (f64, f64), b: (f64, f64)) -> f64 {
        // continuous cell-space coordinates (x → lon cells, y → rows south)
        let (x0, y0) = ((a.0 - self.lon0) / self.dlon, (self.lat0 - a.1) / self.dlat);
        let (x1, y1) = ((b.0 - self.lon0) / self.dlon, (self.lat0 - b.1) / self.dlat);
        #[expect(clippy::cast_possible_truncation, reason = "floored cell coords fit i64")]
        let (mut ix, mut iy) = (x0.floor() as i64, y0.floor() as i64);
        #[expect(clippy::cast_possible_truncation, reason = "floored cell coords fit i64")]
        let (ex, ey) = (x1.floor() as i64, y1.floor() as i64);
        let mut best = self.cell_val(ix, iy);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let (sx, sy) = (if dx > 0.0 { 1 } else { -1 }, if dy > 0.0 { 1 } else { -1 });
        // param t along the segment at the next x/y cell-boundary crossing
        let (mut tx, tdx) = if dx == 0.0 {
            (f64::INFINITY, f64::INFINITY)
        } else {
            #[expect(clippy::cast_precision_loss, reason = "|ix| ~ raster width ≤ 43200/8 for in-range lon; exact in f64")]
            let first = (ix + i64::from(dx > 0.0)) as f64;
            (((first - x0) / dx).abs().max(0.0), (1.0 / dx).abs())
        };
        let (mut ty, tdy) = if dy == 0.0 {
            (f64::INFINITY, f64::INFINITY)
        } else {
            #[expect(clippy::cast_precision_loss, reason = "|iy| ~ raster height ≤ 21600/8 for in-range lat; exact in f64")]
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
        #[expect(clippy::cast_precision_loss, reason = "raster width w ≤ 43200/8 (GHS-POP downsampled); exact in f64")]
        let ix = if (self.width as f64 * self.dlon - 360.0).abs() < 1e-6 {
            ix.rem_euclid(self.width as i64)
        } else if ix < 0 || ix >= self.width as i64 {
            return 0.0;
        } else {
            ix
        };
        let (ix, iy) =
            (usize::try_from(ix).expect("checked in range"), usize::try_from(iy).expect("checked in range"));
        f64::from(self.cells[iy * self.width + ix])
    }

    /// Decode the GHS-POP `GeoTIFF`, summing 8×8 pixel blocks and converting
    /// population counts to people/km².
    ///
    /// # Errors
    /// I/O or TIFF decode failure, missing geotransform tags, or a sample
    /// format other than f32/f64.
    ///
    /// # Panics
    /// If the source raster's dimensions or chunk count exceed u32 (not
    /// reachable for GHS-POP).
    pub fn from_ghs_pop_tif(tif_path: &Path) -> crate::Result<Self> {
        const KM_PER_DEG: f64 = 111.32;
        let mut dec = Decoder::new(BufReader::new(std::fs::File::open(tif_path)?))?
            .with_limits(Limits::unlimited());
        let (sw, sh) = dec.dimensions()?;
        let (sw, sh) = (sw as usize, sh as usize);
        // geotransform: pixel scale + tiepoint (don't assume ±180/±90 cover)
        let scale = dec.get_tag_f64_vec(Tag::ModelPixelScaleTag)?;
        let tie = dec.get_tag_f64_vec(Tag::ModelTiepointTag)?;
        crate::ensure!(scale.len() >= 2 && tie.len() >= 5, Error::MissingGeotransform);
        let (sdlon, sdlat) = (scale[0], scale[1]);
        let (lon0, lat0) = (tie[3] - tie[0] * sdlon, tie[4] + tie[1] * sdlat);

        let (w, h) = (sw.div_ceil(DOWNSAMPLE), sh.div_ceil(DOWNSAMPLE));
        let mut sums = vec![0f64; w * h];
        let (cw, ch) = dec.chunk_dimensions();
        let (cw, ch) = (cw as usize, ch as usize);
        let across = sw.div_ceil(cw);
        for chunk in 0..u32::try_from(across * sh.div_ceil(ch)).expect("chunk count fits u32") {
            let (x_off, y_off) = ((chunk as usize % across) * cw, (chunk as usize / across) * ch);
            // GDAL writes all-nodata ocean tiles as sparse (offset 0) — skip
            let Ok(data) = dec.read_chunk(chunk) else { continue };
            let (dw, dh) = dec.chunk_data_dimensions(chunk);
            let (dw, dh) = (dw as usize, dh as usize);
            let mut add = |px: usize, py: usize, v: f64| {
                // nodata is -200 → clamp negatives to zero population
                if v > 0.0 {
                    sums[(y_off + py) / DOWNSAMPLE * w + (x_off + px) / DOWNSAMPLE] += v;
                }
            };
            match data {
                DecodingResult::F32(v) => {
                    for py in 0..dh {
                        for px in 0..dw {
                            add(px, py, f64::from(v[py * dw + px]));
                        }
                    }
                }
                DecodingResult::F64(v) => {
                    for py in 0..dh {
                        for px in 0..dw {
                            add(px, py, v[py * dw + px]);
                        }
                    }
                }
                other => return Err(Error::BadSampleFormat { format: format!("{other:?}") }),
            }
        }

        // counts → people/km². 111.32 km/deg with a cos(lat) lon correction
        // is plenty: weighting needs order-of-magnitude density, not
        // demographics. cos clamped at 85° (population there ≈ 0 anyway).
        #[expect(clippy::cast_precision_loss, reason = "DOWNSAMPLE = 8, exact in f64")]
        let (dlon, dlat) = (sdlon * DOWNSAMPLE as f64, sdlat * DOWNSAMPLE as f64);
        #[expect(clippy::cast_possible_truncation, reason = "density → f32 grid cell, rounding is fine")]
        let cells = (0..w * h)
            .map(|i| {
                #[expect(clippy::cast_precision_loss, reason = "row index i/w < h ≤ 21600/8; exact in f64")]
                let lat_c = lat0 - (((i / w) as f64) + 0.5) * dlat;
                let coslat = lat_c.to_radians().cos().max((85f64).to_radians().cos());
                let area = (dlat * KM_PER_DEG) * (dlon * KM_PER_DEG * coslat);
                (sums[i] / area) as f32
            })
            .collect();
        Ok(Self { width: w, height: h, lon0, lat0, dlon, dlat, cells })
    }

    fn write_sidecar(&self, path: &Path) -> crate::Result<()> {
        let tmp = path.with_extension("part");
        let mut f = BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(SIDECAR_MAGIC)?;
        f.write_all(&u32::try_from(self.width).expect("grid width fits u32").to_le_bytes())?;
        f.write_all(&u32::try_from(self.height).expect("grid height fits u32").to_le_bytes())?;
        for v in [self.lon0, self.lat0, self.dlon, self.dlat] {
            f.write_all(&v.to_le_bytes())?;
        }
        for c in &self.cells {
            f.write_all(&c.to_le_bytes())?;
        }
        f.into_inner().map_err(std::io::IntoInnerError::into_error)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    fn read_sidecar(path: &Path) -> crate::Result<Self> {
        let mut file = BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        crate::ensure!(&magic == SIDECAR_MAGIC, Error::BadSidecar("bad magic"));
        let mut word = [0u8; 4];
        file.read_exact(&mut word)?;
        let width = u32::from_le_bytes(word) as usize;
        file.read_exact(&mut word)?;
        let height = u32::from_le_bytes(word) as usize;
        let mut scalar = [0u8; 8];
        let mut geo = [0f64; 4];
        for field in &mut geo {
            file.read_exact(&mut scalar)?;
            *field = f64::from_le_bytes(scalar);
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        crate::ensure!(bytes.len() == width * height * 4, Error::BadSidecar("size mismatch"));
        let cells =
            bytes.chunks_exact(4).map(|cell| f32::from_le_bytes(cell.try_into().unwrap())).collect();
        Ok(Self { width, height, lon0: geo[0], lat0: geo[1], dlon: geo[2], dlat: geo[3], cells })
    }
}

/// Extract the single `.tif` entry from the GHS-POP zip next to it
/// (the tiff decoder needs `Seek`, which zip entries don't offer).
fn extract_tif(zip_path: &Path, cache_dir: &Path) -> crate::Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let name = archive
        .file_names()
        .find(|n| Path::new(n).extension().is_some_and(|e| e.eq_ignore_ascii_case("tif")))
        .ok_or_else(|| Error::NoTif { zip: zip_path.into() })?
        .to_string();
    let out = cache_dir.join(name.rsplit('/').next().unwrap());
    let mut entry = archive.by_name(&name)?;
    let tmp = out.with_extension("part");
    std::io::copy(&mut entry, &mut BufWriter::new(std::fs::File::create(&tmp)?))?;
    std::fs::rename(&tmp, &out)?;
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
        DensityGrid { width: 10, height: 10, lon0: 0.0, lat0: 10.0, dlon: 1.0, dlat: 1.0, cells }
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "cell values stored exactly (0.0/1000.0); approximate equality would weaken the test")]
    fn sample_hits_the_right_cell() {
        let g = grid();
        assert_eq!(g.sample(5.5, 5.5), 1000.0);
        assert_eq!(g.sample(4.5, 5.5), 0.0);
        assert_eq!(g.sample(5.5, 4.5), 0.0);
        assert_eq!(g.sample(-20.0, 5.5), 0.0); // outside (grid isn't 360°-wide)
        assert_eq!(g.sample(5.5, 20.0), 0.0);
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "cell values stored exactly (0.0/1000.0); approximate equality would weaken the test")]
    fn max_along_sees_cells_between_vertices() {
        let g = grid();
        // horizontal crossing: both endpoints in cold cells, hot in between
        assert_eq!(g.max_along((1.5, 5.5), (8.5, 5.5)), 1000.0);
        // diagonal crossing
        assert_eq!(g.max_along((4.2, 4.2), (6.8, 6.8)), 1000.0);
        // parallel misses
        assert_eq!(g.max_along((1.5, 3.5), (8.5, 3.5)), 0.0);
        // degenerate (point) segment
        assert_eq!(g.max_along((5.5, 5.5), (5.5, 5.5)), 1000.0);
        // vertical through the hot column
        assert_eq!(g.max_along((5.5, 1.5), (5.5, 8.5)), 1000.0);
    }

    #[test]
    fn sidecar_roundtrip() {
        let g = grid();
        let dir = std::env::temp_dir().join("utz_density_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SIDECAR_NAME);
        g.write_sidecar(&path).unwrap();
        let r = DensityGrid::read_sidecar(&path).unwrap();
        assert_eq!((r.width, r.height, r.lon0, r.lat0, r.dlon, r.dlat), (10, 10, 0.0, 10.0, 1.0, 1.0));
        assert_eq!(r.cells, g.cells);
        std::fs::remove_file(&path).unwrap();
    }
}
