//! Grid + interned-CSR builder (PLAN.md §10).
//!
//! Two passes over the geometry:
//! 1. edge walk — every cell a ring passes through collects that feature id
//!    (candidate sets; ≥2 candidates = border cell needing PIP);
//! 2. scanline fill on a sub×sub-finer grid — even-odd span fill per polygon
//!    gives per-subcell ownership, aggregated to a dominant zone per cell
//!    (interior fill for the primary array + the `lookup_coarse` answer).

use std::collections::{HashMap, HashSet};

use crate::Feat;

pub const NO_ZONE: u16 = u16::MAX;

pub struct CellGrid {
    pub deg: f64,
    pub ncols: usize,
    pub nrows: usize,
    /// sorted candidate feature ids per cell (from the edge walk; empty = no ring)
    pub sets: Vec<Vec<u16>>,
    /// dominant zone per cell from subcell ownership (`NO_ZONE` if nothing filled)
    pub dominant: Vec<u16>,
    /// per-cell subcell ownership tallies (candidate id -> subcells owned)
    pub tallies: Vec<Vec<(u16, u32)>>,
}

/// Rasterize `feats` onto a `deg`-cell grid; ownership sampled on a grid
/// `sub`× finer (sub=8 at 2° → 0.25° subcells).
///
/// # Panics
///
/// Panics if any coordinate is NaN (scanline crossings become unsortable).
#[must_use]
pub fn build(feats: &[Feat], deg: f64, sub: usize) -> CellGrid {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "deg >= 0.1 so at most 3600 cells; float as saturates")]
    let (ncols, nrows) = ((360.0 / deg).ceil() as usize, (180.0 / deg).ceil() as usize);
    let total = ncols * nrows;

    let sets = edge_walk(feats, deg, ncols, nrows);
    let (fcols, frows) = (ncols * sub, nrows * sub);
    let owner = subcell_owners(feats, deg, sub, fcols, frows);

    // ---- aggregate subcell owners to per-cell tallies + dominant ----
    let mut tallies: Vec<HashMap<u16, u32>> = vec![HashMap::new(); total];
    for fj in 0..frows {
        let cj = fj / sub;
        for fi in 0..fcols {
            let o = owner[fj * fcols + fi];
            if o != NO_ZONE {
                *tallies[cj * ncols + fi / sub].entry(o).or_insert(0) += 1;
            }
        }
    }
    // tie-break by smallest id: HashMap iteration order is seeded per process,
    // and a tie decided by it made the whole container nondeterministic
    let dominant: Vec<u16> = tallies.iter()
        .map(|t| {
            t.iter()
                .max_by_key(|&(&z, &c)| (c, core::cmp::Reverse(z)))
                .map_or(NO_ZONE, |(&z, _)| z)
        })
        .collect();
    let tallies: Vec<Vec<(u16, u32)>> = tallies.into_iter().map(|t| {
        let mut v: Vec<(u16, u32)> = t.into_iter().collect();
        v.sort_unstable();
        v
    }).collect();
    // candidate set = edge walk ∪ subcell owners. The union matters where TZBB
    // zones deliberately OVERLAP (e.g. Asia/Shanghai + Asia/Urumqi over
    // Xinjiang): a zone covering a whole cell leaves no ring in it, so the edge
    // walk alone misses it and would mislabel the cell interior.
    let sets: Vec<Vec<u16>> = sets.into_iter().enumerate().map(|(c, mut s)| {
        s.extend(tallies[c].iter().map(|&(z, _)| z));
        let mut v: Vec<u16> = s.into_iter().collect();
        v.sort_unstable();
        v
    }).collect();

    CellGrid { deg, ncols, nrows, sets, dominant, tallies }
}


/// Pass 1: walk every ring edge in `deg`-sized steps — every cell an edge
/// passes through collects that feature id (candidate sets; ≥2 candidates =
/// border cell needing PIP).
fn edge_walk(feats: &[Feat], deg: f64, ncols: usize, nrows: usize) -> Vec<HashSet<u16>> {
    let mut sets: Vec<HashSet<u16>> = vec![HashSet::new(); ncols * nrows];
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, reason = "cast saturates then clamped to grid range")]
    let cell = |lon: f64, lat: f64| -> usize {
        let c = (((lon + 180.0) / deg) as isize).clamp(0, ncols as isize - 1) as usize;
        let r = (((lat + 90.0) / deg) as isize).clamp(0, nrows as isize - 1) as usize;
        r * ncols + c
    };
    for (fid, f) in feats.iter().enumerate() {
        for p in &f.polys {
            for ring in p {
                let n = ring.len();
                for i in 0..n {
                    let (x0, y0) = ring[i];
                    let (x1, y1) = ring[(i + 1) % n];
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "span/deg bounded by world size; float as saturates")]
                    let steps = ((((x1 - x0).abs()).max((y1 - y0).abs()) / deg * 2.0).ceil() as usize).max(1);
                    for s in 0..=steps {
                        #[expect(clippy::cast_precision_loss, reason = "s ≤ steps ≤ 2·360/deg ≪ 2^53; interpolation parameter")]
                        let t = s as f64 / steps as f64;
                        sets[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)].insert(u16::try_from(fid).expect("feature id fits u16"));
                    }
                }
            }
        }
    }
    sets
}

/// Pass 2: even-odd scanline fill per polygon on the `sub`×-finer grid —
/// per-subcell ownership (later aggregated to a dominant zone per cell).
///
/// # Panics
/// If any coordinate is NaN (crossing xs become unsortable).
fn subcell_owners(feats: &[Feat], deg: f64, sub: usize, fcols: usize, frows: usize) -> Vec<u16> {
    #[expect(clippy::cast_precision_loss, reason = "subdivision factor sub = 8 in practice, ≪ 2^53; exact in f64")]
    let r = deg / sub as f64;
    let mut owner: Vec<u16> = vec![NO_ZONE; fcols * frows];
    let mut row_x: Vec<Vec<f32>> = vec![Vec::new(); frows]; // crossing xs per row, reused per poly
    for (fid, f) in feats.iter().enumerate() {
        for p in &f.polys {
            // bucket edge crossings of every ring (exterior + holes) by row: even-odd
            let mut touched: Vec<u32> = Vec::new();
            for ring in p {
                let n = ring.len();
                for i in 0..n {
                    let (x0, y0) = ring[i];
                    let (x1, y1) = ring[(i + 1) % n];
                    #[expect(clippy::float_cmp, reason = "skip exactly-horizontal edges before dividing by y1-y0; near-horizontal must still cross")]
                    if y0 == y1 { continue; }
                    let (ylo, yhi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                    // rows whose center lat is in [ylo, yhi)
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "row index bounded to [0, frows); float as saturates")]
                    let j0 = (((ylo + 90.0) / r - 0.5).ceil().max(0.0)) as usize;
                    #[expect(clippy::cast_possible_truncation, reason = "row index bounded to [0, frows); float as saturates")]
                    #[expect(clippy::cast_precision_loss, reason = "frows = nrows*sub ≤ 8*1800; exact in f64")]
                    let j1 = (((yhi + 90.0) / r - 0.5).floor().min(frows as f64 - 1.0)) as isize;
                    let mut j = j0.cast_signed();
                    while j <= j1 {
                        #[expect(clippy::cast_precision_loss, reason = "row index j < frows ≤ 8*1800; exact in f64")]
                        let lat = -90.0 + (j as f64 + 0.5) * r;
                        if lat >= ylo && lat < yhi {
                            let x = x0 + (lat - y0) / (y1 - y0) * (x1 - x0);
                            if row_x[j.cast_unsigned()].is_empty() { touched.push(u32::try_from(j).expect("row index fits u32")); }
                            #[expect(clippy::cast_possible_truncation, reason = "crossing x stored at f32 by design (row_x)")]
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
                    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "col index bounded to [0, fcols); float as saturates")]
                    let i0 = (((xa + 180.0) / r - 0.5).ceil().max(0.0)) as usize;
                    #[expect(clippy::cast_possible_truncation, reason = "col index bounded to [0, fcols); float as saturates")]
                    #[expect(clippy::cast_precision_loss, reason = "fcols = ncols*sub ≤ 8*3600; exact in f64")]
                    let i1 = (((xb + 180.0) / r - 0.5).floor().min(fcols as f64 - 1.0)) as isize;
                    let base = j as usize * fcols;
                    let mut i = i0.cast_signed();
                    while i <= i1 {
                        owner[base + i.cast_unsigned()] = u16::try_from(fid).expect("feature id fits u16");
                        i += 1;
                    }
                }
                xs.clear();
            }
        }
    }
    owner
}

/// Candidate-list ordering inside the interned CSR (PLAN.md §10 dominant-first).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Order {
    /// ascending feature id — maximal interning (baseline)
    IdSorted,
    /// descending global zone area — deterministic per set, same interning as `IdSorted`
    AreaDesc,
    /// this cell's dominant zone first, rest id-sorted — best early-exit, breaks interning
    CellDominantFirst,
}

pub struct Csr {
    /// u16 per cell: high bit 0 = zone id (or `NO_ZONE` marker semantics left to
    /// the container), high bit 1 = index into the interned lists
    pub primary: Vec<u16>,
    pub list_offsets: Vec<u16>,
    pub list_ids: Vec<u16>,
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
    let total = grid.ncols * grid.nrows;
    let mut primary = vec![0u16; total];
    let mut lists: Vec<Vec<u16>> = Vec::new();
    let mut index: HashMap<Vec<u16>, u16> = HashMap::new();
    let by_area = |v: &mut Vec<u16>| {
        v.sort_unstable_by(|&a, &b| areas[b as usize].partial_cmp(&areas[a as usize]).unwrap()
            .then(a.cmp(&b)));
    };
    for (c, pc) in primary.iter_mut().enumerate() {
        let set = &grid.sets[c];
        if set.len() > 1 {
            let mut list = set.clone(); // already id-sorted
            match order {
                Order::IdSorted => {}
                Order::AreaDesc => by_area(&mut list),
                Order::CellDominantFirst => {
                    let dom = grid.dominant[c];
                    if let Some(pos) = list.iter().position(|&z| z == dom) {
                        list.remove(pos);
                        list.insert(0, dom);
                    }
                }
            }
            let next = u16::try_from(lists.len()).expect("interned list index fits u16 (encode re-checks 15-bit)");
            let li = *index.entry(list.clone()).or_insert_with(|| { lists.push(list); next });
            *pc = 0x8000 | li;
        } else {
            // interior (single candidate) or no-ring cell: dominant zone
            let z = if set.len() == 1 { set[0] } else { grid.dominant[c] };
            *pc = if z == NO_ZONE { 0x7FFF } else { z };
        }
    }
    let mut list_offsets = Vec::with_capacity(lists.len() + 1);
    let mut list_ids = Vec::new();
    list_offsets.push(0u16);
    for l in &lists {
        list_ids.extend_from_slice(l);
        list_offsets.push(u16::try_from(list_ids.len()).expect("list ids fit u16 offsets (encode re-checks)"));
    }
    Csr { primary, list_offsets, list_ids, uniq_lists: lists.len() }
}

/// Approximate global area per feature (equirectangular shoelace with cos-lat
/// correction; exteriors minus holes, clamped ≥ 0). Ranking only.
#[must_use]
pub fn feat_areas(feats: &[Feat]) -> Vec<f64> {
    feats.iter().map(|f| {
        let mut a = 0.0f64;
        for p in &f.polys {
            for (ri, ring) in p.iter().enumerate() {
                let ra = ring_area(ring);
                if ri == 0 { a += ra; } else { a -= ra; }
            }
        }
        a.max(0.0)
    }).collect()
}

fn ring_area(ring: &[(f64, f64)]) -> f64 {
    if ring.len() < 3 { return 0.0; }
    #[expect(clippy::cast_precision_loss, reason = "ring.len() ≪ 2^53; mean latitude")]
    let midlat = ring.iter().map(|&(_, y)| y).sum::<f64>() / ring.len() as f64;
    let mut s = 0.0;
    for i in 0..ring.len() {
        let (x0, y0) = ring[i];
        let (x1, y1) = ring[(i + 1) % ring.len()];
        s += x0 * y1 - x1 * y0;
    }
    (s.abs() / 2.0) * midlat.to_radians().cos().abs()
}
