//! Grid + interned-CSR builder.
//!
//! Two passes over the geometry:
//! 1. edge walk: every cell a ring passes through collects that feature id
//!    (candidate sets; ≥2 candidates = border cell needing PIP);
//! 2. scanline fill on a subdivision-finer grid: even-odd span fill per polygon
//!    gives per-subcell ownership, aggregated to a dominant zone per cell
//!    (interior fill for the primary array + the `lookup_coarse` answer).

use std::collections::{HashMap, HashSet};

use ndarray::Array2;

use crate::Feat;
use utz_common::NO_ZONE;

/// Cell arrays are `[row, col]`-indexed and iterate row-major, the same
/// order the primary table serializes in.
pub struct CellGrid {
    pub deg: f64,
    /// sorted candidate feature ids per cell (from the edge walk; empty = no ring)
    pub sets: Array2<Vec<u16>>,
    /// dominant zone per cell from subcell ownership (`NO_ZONE` if nothing filled)
    pub dominant: Array2<u16>,
    /// per-cell subcell ownership tallies (candidate id -> subcells owned)
    pub tallies: Array2<Vec<(u16, u32)>>,
}

impl CellGrid {
    #[must_use]
    pub fn ncols(&self) -> usize {
        self.dominant.ncols()
    }

    #[must_use]
    pub fn nrows(&self) -> usize {
        self.dominant.nrows()
    }
}

/// Rasterize `feats` onto a `deg`-cell grid; ownership sampled on a grid
/// `subdivision`× finer (subdivision=8 at 2° → 0.25° subcells).
///
/// # Panics
///
/// Panics if any coordinate is NaN (scanline crossings become unsortable).
#[must_use]
pub fn build(feats: &[Feat], deg: f64, subdivision: usize) -> CellGrid {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "deg >= 0.1 so at most 3600 cells; float as saturates"
    )]
    let (ncols, nrows) = ((360.0 / deg).ceil() as usize, (180.0 / deg).ceil() as usize);

    let mut sets = edge_walk(feats, deg, ncols, nrows);
    let owner = subcell_owners(
        feats,
        deg,
        subdivision,
        ncols * subdivision,
        nrows * subdivision,
    );

    // ---- aggregate subcell owners to per-cell tallies + dominant ----
    let mut counts: Array2<HashMap<u16, u32>> = Array2::from_elem((nrows, ncols), HashMap::new());
    for ((fine_row, fine_col), &zone) in owner.indexed_iter() {
        if zone != NO_ZONE {
            *counts[[fine_row / subdivision, fine_col / subdivision]]
                .entry(zone)
                .or_insert(0) += 1;
        }
    }
    // tie-break by smallest id: HashMap iteration order is seeded per process,
    // and a tie decided by it made the whole container nondeterministic
    let dominant: Array2<u16> = counts.map(|tally| {
        tally
            .iter()
            .max_by_key(|&(&zone, &count)| (count, core::cmp::Reverse(zone)))
            .map_or(NO_ZONE, |(&zone, _)| zone)
    });
    let tallies: Array2<Vec<(u16, u32)>> = counts.map(|tally| {
        let mut entries: Vec<(u16, u32)> =
            tally.iter().map(|(&zone, &count)| (zone, count)).collect();
        entries.sort_unstable();
        entries
    });
    // candidate set = edge walk ∪ subcell owners. The union matters where TZBB
    // zones deliberately OVERLAP (e.g. Asia/Shanghai + Asia/Urumqi over
    // Xinjiang): a zone covering a whole cell leaves no ring in it, so the edge
    // walk alone misses it and would mislabel the cell interior.
    for (set, tally) in sets.iter_mut().zip(tallies.iter()) {
        set.extend(tally.iter().map(|&(zone, _)| zone));
    }
    let sets: Array2<Vec<u16>> = sets.map(|set| {
        let mut sorted_ids: Vec<u16> = set.iter().copied().collect();
        sorted_ids.sort_unstable();
        sorted_ids
    });

    CellGrid {
        deg,
        sets,
        dominant,
        tallies,
    }
}

/// Pass 1: walk every ring edge in `deg`-sized steps. Every cell an edge
/// passes through collects that feature id (candidate sets; ≥2 candidates =
/// border cell needing PIP).
fn edge_walk(feats: &[Feat], deg: f64, ncols: usize, nrows: usize) -> Array2<HashSet<u16>> {
    let mut sets: Array2<HashSet<u16>> = Array2::from_elem((nrows, ncols), HashSet::new());
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "cast saturates then clamped to grid range"
    )]
    let cell = |lon: f64, lat: f64| -> [usize; 2] {
        let col = (((lon + 180.0) / deg) as isize).clamp(0, ncols as isize - 1) as usize;
        let row = (((lat + 90.0) / deg) as isize).clamp(0, nrows as isize - 1) as usize;
        [row, col]
    };
    for (feature_id, feature) in feats.iter().enumerate() {
        for poly in &feature.polys {
            for ring in poly {
                let ring_len = ring.len();
                for i in 0..ring_len {
                    let (x0, y0) = ring[i];
                    let (x1, y1) = ring[(i + 1) % ring_len];
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "span/deg bounded by world size; float as saturates"
                    )]
                    let steps = ((((x1 - x0).abs()).max((y1 - y0).abs()) / deg * 2.0).ceil()
                        as usize)
                        .max(1);
                    for step in 0..=steps {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "step ≤ steps ≤ 2·360/deg ≪ 2^53; interpolation parameter"
                        )]
                        let t = step as f64 / steps as f64;
                        sets[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)]
                            .insert(u16::try_from(feature_id).expect("feature id fits u16"));
                    }
                }
            }
        }
    }
    sets
}

/// Pass 2: even-odd scanline fill per polygon on the `subdivision`×-finer
/// grid, yielding per-subcell ownership (later aggregated to a dominant zone
/// per cell).
///
/// # Panics
/// If any coordinate is NaN (crossing xs become unsortable).
fn subcell_owners(
    feats: &[Feat],
    deg: f64,
    subdivision: usize,
    fine_cols: usize,
    fine_rows: usize,
) -> Array2<u16> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "subdivision factor = 8 in practice, ≪ 2^53; exact in f64"
    )]
    let subcell_deg = deg / subdivision as f64;
    let mut owner: Array2<u16> = Array2::from_elem((fine_rows, fine_cols), NO_ZONE);
    // crossing xs per row, reused per poly
    let mut row_x: Vec<Vec<f32>> = vec![Vec::new(); fine_rows];
    for (feature_id, feature) in feats.iter().enumerate() {
        for poly in &feature.polys {
            // bucket edge crossings of every ring (exterior + holes) by row: even-odd
            let mut touched: Vec<u32> = Vec::new();
            for ring in poly {
                let ring_len = ring.len();
                for i in 0..ring_len {
                    let (x0, y0) = ring[i];
                    let (x1, y1) = ring[(i + 1) % ring_len];
                    #[expect(
                        clippy::float_cmp,
                        reason = "skip exactly-horizontal edges before dividing by y1-y0; near-horizontal must still cross"
                    )]
                    if y0 == y1 {
                        continue;
                    }
                    let (ylo, yhi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                    // rows whose center lat is in [ylo, yhi)
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "row index bounded to [0, fine_rows); float as saturates"
                    )]
                    let j0 = (((ylo + 90.0) / subcell_deg - 0.5).ceil().max(0.0)) as usize;
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "row index bounded to [0, fine_rows); float as saturates"
                    )]
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "fine_rows = nrows*subdivision ≤ 8*1800; exact in f64"
                    )]
                    let j1 = (((yhi + 90.0) / subcell_deg - 0.5)
                        .floor()
                        .min(fine_rows as f64 - 1.0)) as isize;
                    let mut j = j0.cast_signed();
                    while j <= j1 {
                        #[expect(
                            clippy::cast_precision_loss,
                            reason = "row index j < fine_rows ≤ 8*1800; exact in f64"
                        )]
                        let lat = -90.0 + (j as f64 + 0.5) * subcell_deg;
                        if lat >= ylo && lat < yhi {
                            let x = x0 + (lat - y0) / (y1 - y0) * (x1 - x0);
                            if row_x[j.cast_unsigned()].is_empty() {
                                touched.push(u32::try_from(j).expect("row index fits u32"));
                            }
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "crossing x stored at f32 by design (row_x)"
                            )]
                            row_x[j.cast_unsigned()].push(x as f32);
                        }
                        j += 1;
                    }
                }
            }
            // fill alternate spans
            for &j in &touched {
                let xs = &mut row_x[j as usize];
                xs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
                for pair in xs.chunks_exact(2) {
                    let (xa, xb) = (f64::from(pair[0]), f64::from(pair[1]));
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "col index bounded to [0, fine_cols); float as saturates"
                    )]
                    let i0 = (((xa + 180.0) / subcell_deg - 0.5).ceil().max(0.0)) as usize;
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "col index bounded to [0, fine_cols); float as saturates"
                    )]
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "fine_cols = ncols*subdivision ≤ 8*3600; exact in f64"
                    )]
                    let i1 = (((xb + 180.0) / subcell_deg - 0.5)
                        .floor()
                        .min(fine_cols as f64 - 1.0)) as isize;
                    let mut row = owner.row_mut(j as usize);
                    let mut i = i0.cast_signed();
                    while i <= i1 {
                        row[i.cast_unsigned()] =
                            u16::try_from(feature_id).expect("feature id fits u16");
                        i += 1;
                    }
                }
                xs.clear();
            }
        }
    }
    owner
}

/// Candidate-list ordering inside the interned CSR.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Order {
    /// ascending feature id: maximal interning (baseline)
    IdSorted,
    /// descending global zone area: deterministic per set, same interning as `IdSorted`
    AreaDesc,
    /// this cell's dominant zone first, rest id-sorted: best early-exit, breaks interning
    CellDominantFirst,
}

/// The serializable grid prefilter: one u16 per cell plus the interned
/// candidate lists in compressed-sparse-row form.
pub struct Csr {
    /// u16 per cell: high bit 0 = zone id (or `NO_ZONE` marker semantics left to
    /// the container), high bit 1 = index into the interned lists
    pub primary: Vec<u16>,
    /// Start offset of each interned list in [`Csr::list_ids`].
    pub list_offsets: Vec<u16>,
    /// The interned candidate lists, concatenated.
    pub list_ids: Vec<u16>,
    /// Distinct interned lists.
    pub uniq_lists: usize,
}

impl Csr {
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.primary.len() * 2 + self.list_offsets.len() * 2 + self.list_ids.len() * 2
    }
}

/// Build the interned CSR. `areas` (global zone area, any consistent unit) is
/// used by `AreaDesc`/`CellDominantFirst`.
///
/// # Panics
///
/// Panics with `AreaDesc` if a candidate's `areas` entry is NaN.
#[must_use]
pub fn intern_csr(grid: &CellGrid, order: Order, areas: &[f64]) -> Csr {
    let mut primary = vec![0u16; grid.dominant.len()];
    let mut lists: Vec<Vec<u16>> = Vec::new();
    let mut index: HashMap<Vec<u16>, u16> = HashMap::new();
    let by_area = |list: &mut Vec<u16>| {
        list.sort_unstable_by(|&a, &b| {
            areas[b as usize]
                .partial_cmp(&areas[a as usize])
                .unwrap()
                .then(a.cmp(&b))
        });
    };
    // row-major zip: primary cell c ↔ the arrays' cell c
    for ((set, &dominant), primary_cell) in grid
        .sets
        .iter()
        .zip(grid.dominant.iter())
        .zip(primary.iter_mut())
    {
        if set.len() > 1 {
            let mut list = set.clone(); // already id-sorted
            match order {
                Order::IdSorted => {}
                Order::AreaDesc => by_area(&mut list),
                Order::CellDominantFirst => {
                    if let Some(position) = list.iter().position(|&zone| zone == dominant) {
                        list.remove(position);
                        list.insert(0, dominant);
                    }
                }
            }
            let next = u16::try_from(lists.len())
                .expect("interned list index fits u16 (encode re-checks 15-bit)");
            let list_index = *index.entry(list.clone()).or_insert_with(|| {
                lists.push(list);
                next
            });
            *primary_cell = 0x8000 | list_index;
        } else {
            // interior (single candidate) or no-ring cell: dominant zone
            let zone = if set.len() == 1 { set[0] } else { dominant };
            // an unclaimed cell already carries the wire marker (ids stay
            // below NO_ZONE by the 15-bit guard)
            *primary_cell = zone;
        }
    }
    let mut list_offsets = Vec::with_capacity(lists.len() + 1);
    let mut list_ids = Vec::new();
    list_offsets.push(0u16);
    for list in &lists {
        list_ids.extend_from_slice(list);
        list_offsets.push(
            u16::try_from(list_ids.len()).expect("list ids fit u16 offsets (encode re-checks)"),
        );
    }
    Csr {
        primary,
        list_offsets,
        list_ids,
        uniq_lists: lists.len(),
    }
}

/// Approximate global area per feature (equirectangular shoelace with cos-lat
/// correction; exteriors minus holes, clamped ≥ 0). Ranking only.
#[must_use]
pub fn feat_areas(feats: &[Feat]) -> Vec<f64> {
    feats
        .iter()
        .map(|feature| {
            let mut area = 0.0f64;
            for poly in &feature.polys {
                for (ring_index, ring) in poly.iter().enumerate() {
                    let ring_contribution = ring_area(ring);
                    if ring_index == 0 {
                        area += ring_contribution;
                    } else {
                        area -= ring_contribution;
                    }
                }
            }
            area.max(0.0)
        })
        .collect()
}

fn ring_area(ring: &[(f64, f64)]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "ring.len() ≪ 2^53; mean latitude"
    )]
    let midlat = ring.iter().map(|&(_, y)| y).sum::<f64>() / ring.len() as f64;
    let mut sum = 0.0;
    for i in 0..ring.len() {
        let (x0, y0) = ring[i];
        let (x1, y1) = ring[(i + 1) % ring.len()];
        sum += x0 * y1 - x1 * y0;
    }
    (sum.abs() / 2.0) * midlat.to_radians().cos().abs()
}
