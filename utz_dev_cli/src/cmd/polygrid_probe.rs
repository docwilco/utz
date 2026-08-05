//! Would a poly-granular grid replace the per-poly bboxes?
//!
//! Rebuilds the grid from a codec-*none* asset's geometry twice with the
//! real builder (`grid::build()` + `intern_csr()`): once per feature (today's
//! format) and once with each polygon exploded into its own pseudo-feature
//! (the "purely grid" design: border-cell lists reference polys directly,
//! so lazy PIP jumps straight to the polys that touch the cell and the
//! per-poly bbox becomes redundant).
//!
//! Reports the CSR cost of the finer lists (unique-list count vs the 15-bit
//! tag space, `list_ids` growth), the pruning waste it would eliminate (polys
//! parsed per border-cell candidate today vs polys actually near the cell),
//! and the net size delta (grid growth − dropped bboxes + poly→feature
//! table). Also builds the feature grid at deg/2 and deg/4 for the
//! "grid-only exact answers" scaling curve.
//!
//! ```text
//! utz_dev_cli polygrid-probe <none.utz>...
//! ```

use utz::format::{self, read_fixed, read_u16, read_u32, read_varint, unzigzag};
use utz_build::Feat;
use utz_common::GeomEncoding;
use utz_encode::grid::{self, Order};

/// Decodes one arc (forward orientation) into (i32, i32) coords.
fn arc_coords(payload: &[u8], header: &format::PayloadLayout, id: usize) -> Vec<(i32, i32)> {
    let coord_bytes = header.quant_bits.bytes();
    let mut position = header.arc_data + read_u32(payload, header.arc_offsets + id * 4) as usize;
    let (vcount, after_vcount) = read_varint(payload, position);
    position = after_vcount;
    let mut coords = Vec::with_capacity(usize::try_from(vcount).expect("vcount fits usize"));
    if header.geom == GeomEncoding::FixedWidthArcs {
        for _ in 0..vcount {
            coords.push((
                read_fixed(payload, position, header.quant_bits),
                read_fixed(payload, position + coord_bytes, header.quant_bits),
            ));
            position += 2 * coord_bytes;
        }
        return coords;
    }
    let mut qlon = i64::from(read_fixed(payload, position, header.quant_bits));
    let mut qlat = i64::from(read_fixed(
        payload,
        position + coord_bytes,
        header.quant_bits,
    ));
    position += 2 * coord_bytes;
    let to_i32 = |coord: i64| i32::try_from(coord).expect("quantized coord fits i32");
    coords.push((to_i32(qlon), to_i32(qlat)));
    for _ in 1..vcount {
        let (dlon, after_dlon) = read_varint(payload, position);
        let (dlat, after_dlat) = read_varint(payload, after_dlon);
        position = after_dlat;
        qlon += unzigzag(dlon);
        qlat += unzigzag(dlat);
        coords.push((to_i32(qlon), to_i32(qlat)));
    }
    coords
}

/// Loads a container into per-feature dequantized geometry (the same
/// dequantization as the encoder).
fn load_feats(bytes: &[u8]) -> (format::PayloadLayout, Vec<Feat>) {
    let start = format::outer(bytes).expect("not a utz container");
    let payload = &bytes[start + format::PAYLOAD_HEADER_LEN..];
    let header = format::parse(&bytes[start..]).unwrap();
    assert_eq!(
        header.codec,
        utz::Codec::Uncompressed,
        "need a codec-none container"
    );
    assert!(
        matches!(
            header.geom,
            GeomEncoding::VarintArcs | GeomEncoding::FixedWidthArcs
        ),
        "arc-store containers only (geom 0/1)"
    );
    #[expect(
        clippy::cast_precision_loss,
        reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64"
    )]
    let qmax = ((1u64 << (header.quant_bits.bits() - 1)) - 1) as f64;
    let dequantize = |value: i32, half: f64| f64::from(value) / qmax * half;
    let mut features: Vec<Feat> = (0..header.n_features)
        .map(|_| Feat {
            offset: 0.0,
            tzid: None,
            polys: Vec::new(),
        })
        .collect();
    let coord_bytes = header.quant_bits.bytes();
    for pid in 0..header.eager_polys as usize {
        let feature_index = read_u16(payload, header.parent + pid * 2) as usize;
        let mut position =
            header.ring_data + read_u32(payload, header.poly_offsets + pid * 4) as usize;
        position += 4 * coord_bytes; // per-poly bbox (v5)
        let nrings = read_u16(payload, position);
        position += 2;
        let mut rings = Vec::with_capacity(nrings as usize);
        for _ in 0..nrings {
            let (nrefs, mut ref_position) = read_varint(payload, position);
            let mut ring: Vec<(f64, f64)> = Vec::new();
            for _ in 0..nrefs {
                let (arc_ref, next_position) = read_varint(payload, ref_position);
                ref_position = next_position;
                let (id, reversed) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
                let mut arc = arc_coords(payload, &header, id);
                if reversed {
                    arc.reverse();
                }
                ring.extend(
                    arc.iter()
                        .map(|&(x, y)| (dequantize(x, 180.0), dequantize(y, 90.0))),
                );
            }
            position = ref_position;
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            rings.push(ring);
        }
        features[feature_index].polys.push(rings);
    }
    (header, features)
}

struct Stats {
    border_cells: usize,
    uniq_lists: usize,
    list_ids: usize,
    csr_bytes: usize,
    avg_list: f64,
    max_list: usize,
}

fn measure(features: &[Feat], deg: f64) -> (grid::CellGrid, Stats) {
    let grid = grid::build(features, deg, 8);
    let areas = grid::feat_areas(features);
    let csr = grid::intern_csr(&grid, Order::CellDominantFirst, &areas);
    let border: Vec<&Vec<u16>> = grid.sets.iter().filter(|set| set.len() > 1).collect();
    #[expect(
        clippy::cast_precision_loss,
        reason = "candidate-id sum and border-cell count ≪ 2^53; avg display"
    )]
    let avg_list =
        border.iter().map(|set| set.len()).sum::<usize>() as f64 / border.len().max(1) as f64;
    let stats = Stats {
        border_cells: border.len(),
        uniq_lists: csr.uniq_lists,
        list_ids: csr.list_ids.len(),
        csr_bytes: csr.bytes(),
        avg_list,
        max_list: border.iter().map(|set| set.len()).max().unwrap_or(0),
    };
    (grid, stats)
}

#[derive(clap::Args)]
pub struct Args {
    /// The codec-none .utz asset path(s).
    #[arg(required = true)]
    paths: Vec<String>,
}

/// # Errors
/// The command fails on an I/O error reading an input asset.
///
/// # Panics
/// The command panics if an input is not a codec-none arc-store .utz
/// asset (geom 0/1), if its path has no file stem, or if it holds more
/// features than fit a `u16` id.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: &Args) -> utz_build::Result<()> {
    for path in &args.paths {
        let bytes = std::fs::read(path)?;
        let (header, features) = load_feats(&bytes);
        let deg = f64::from(header.grid_deg);
        let coord_bytes = header.quant_bits.bytes();
        let n_polys: usize = features.iter().map(|feature| feature.polys.len()).sum();
        let polys_per_feat: Vec<usize> =
            features.iter().map(|feature| feature.polys.len()).collect();

        // explode: one pseudo-feature per polygon, remember the parent
        let mut poly_features = Vec::with_capacity(n_polys);
        let mut parent = Vec::with_capacity(n_polys);
        for (feature_index, feature) in features.iter().enumerate() {
            for poly in &feature.polys {
                poly_features.push(Feat {
                    offset: 0.0,
                    tzid: None,
                    polys: vec![poly.clone()],
                });
                parent.push(u16::try_from(feature_index).expect("feature id fits u16"));
            }
        }

        let (feature_grid, feature_stats) = measure(&features, deg);
        let (poly_grid, poly_stats) = measure(&poly_features, deg);

        // pruning waste today: in each border cell, candidate features drag
        // in ALL their polys (bbox-checked + ref-lists parsed); the poly
        // grid would visit only the polys whose rings touch the cell
        let (mut today_poly_visits, mut grid_poly_visits) = (0usize, 0usize);
        for (feature_cell, poly_cell) in feature_grid.sets.iter().zip(poly_grid.sets.iter()) {
            if feature_cell.len() > 1 {
                today_poly_visits += feature_cell
                    .iter()
                    .map(|&fid| polys_per_feat[fid as usize])
                    .sum::<usize>();
                grid_poly_visits += poly_cell
                    .iter()
                    .filter(|&&poly_index| {
                        // count only polys of candidate features (same PIP set)
                        feature_cell.contains(&parent[poly_index as usize])
                    })
                    .count();
            }
        }

        // net size: grid growth − dropped bboxes + poly→feature id table
        // (ring records become directly addressable via the existing
        // feat_offsets-style table reshaped per poly; u16 parent id per poly)
        let bbox_bytes = n_polys * 4 * coord_bytes;
        let parent_bytes = n_polys * 2;
        let delta = poly_stats.csr_bytes.cast_signed()
            - feature_stats.csr_bytes.cast_signed()
            - bbox_bytes.cast_signed()
            + parent_bytes.cast_signed();

        let name = std::path::Path::new(&path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        println!(
            "== {name}: {} feats, {n_polys} polys, {deg:.3}° grid ==",
            features.len()
        );
        println!(
            "  border cells        feature-grid {:6}   poly-grid {:6}",
            feature_stats.border_cells, poly_stats.border_cells
        );
        println!(
            "  uniq lists (cap {}) {:9}          {:9}",
            utz_common::NO_ZONE,
            feature_stats.uniq_lists,
            poly_stats.uniq_lists
        );
        println!(
            "  list_ids (cap 65535)   {:9}          {:9}",
            feature_stats.list_ids, poly_stats.list_ids
        );
        println!(
            "  csr bytes              {:9}          {:9}",
            feature_stats.csr_bytes, poly_stats.csr_bytes
        );
        println!(
            "  avg/max list           {:5.2}/{:3}          {:5.2}/{:3}",
            feature_stats.avg_list,
            feature_stats.max_list,
            poly_stats.avg_list,
            poly_stats.max_list
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "poly and border-cell counts ≪ 2^53; ratio display"
        )]
        let (per_today, per_grid, fewer) = (
            today_poly_visits as f64 / feature_stats.border_cells.max(1) as f64,
            grid_poly_visits as f64 / feature_stats.border_cells.max(1) as f64,
            today_poly_visits as f64 / grid_poly_visits.max(1) as f64,
        );
        println!(
            "  polys per border lookup: {per_today:.1} today (bbox-pruned) vs {per_grid:.1} poly-grid ({fewer:.1}x fewer)"
        );
        println!(
            "  net size: csr {:+} − bboxes {} + parents {} = {:+} bytes",
            poly_stats.csr_bytes.cast_signed() - feature_stats.csr_bytes.cast_signed(),
            bbox_bytes,
            parent_bytes,
            delta
        );

        // grid-only-exact scaling: how fast do border cells shrink?
        print!("  grid-only scaling (feature grid): ");
        for scaled_deg in [deg, deg / 2.0, deg / 4.0] {
            let (_, stats) = measure(&features, scaled_deg);
            print!(
                "{scaled_deg:.3}°: {} border cells / {} KiB csr;  ",
                stats.border_cells,
                stats.csr_bytes / 1024
            );
        }
        println!();
    }
    Ok(())
}
