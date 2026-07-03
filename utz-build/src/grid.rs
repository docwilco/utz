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
    /// dominant zone per cell from subcell ownership (NO_ZONE if nothing filled)
    pub dominant: Vec<u16>,
    /// per-cell subcell ownership tallies (candidate id -> subcells owned)
    pub tallies: Vec<Vec<(u16, u32)>>,
}

/// Rasterize `feats` onto a `deg`-cell grid; ownership sampled on a grid
/// `sub`× finer (sub=8 at 2° → 0.25° subcells).
pub fn build(feats: &[Feat], deg: f64, sub: usize) -> CellGrid {
    let ncols = (360.0 / deg).ceil() as usize;
    let nrows = (180.0 / deg).ceil() as usize;
    let total = ncols * nrows;

    // ---- pass 1: edge walk -> candidate sets ----
    let mut sets: Vec<HashSet<u16>> = vec![HashSet::new(); total];
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
                    let steps = ((((x1 - x0).abs()).max((y1 - y0).abs()) / deg * 2.0).ceil() as usize).max(1);
                    for s in 0..=steps {
                        let t = s as f64 / steps as f64;
                        sets[cell(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)].insert(fid as u16);
                    }
                }
            }
        }
    }

    // ---- pass 2: scanline fill on the fine grid -> subcell owners ----
    let fcols = ncols * sub;
    let frows = nrows * sub;
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
                    if y0 == y1 { continue; }
                    let (ylo, yhi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                    // rows whose center lat is in [ylo, yhi)
                    let j0 = (((ylo + 90.0) / r - 0.5).ceil().max(0.0)) as usize;
                    let j1 = (((yhi + 90.0) / r - 0.5).floor().min(frows as f64 - 1.0)) as isize;
                    let mut j = j0 as isize;
                    while j <= j1 {
                        let lat = -90.0 + (j as f64 + 0.5) * r;
                        if lat >= ylo && lat < yhi {
                            let x = x0 + (lat - y0) / (y1 - y0) * (x1 - x0);
                            if row_x[j as usize].is_empty() { touched.push(j as u32); }
                            row_x[j as usize].push(x as f32);
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
                    let (xa, xb) = (pair[0] as f64, pair[1] as f64);
                    let i0 = (((xa + 180.0) / r - 0.5).ceil().max(0.0)) as usize;
                    let i1 = (((xb + 180.0) / r - 0.5).floor().min(fcols as f64 - 1.0)) as isize;
                    let base = j as usize * fcols;
                    let mut i = i0 as isize;
                    while i <= i1 {
                        owner[base + i as usize] = fid as u16;
                        i += 1;
                    }
                }
                xs.clear();
            }
        }
    }

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
    let dominant: Vec<u16> = tallies.iter()
        .map(|t| t.iter().max_by_key(|(_, &c)| c).map(|(&z, _)| z).unwrap_or(NO_ZONE))
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

/// Candidate-list ordering inside the interned CSR (PLAN.md §10 dominant-first).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Order {
    /// ascending feature id — maximal interning (baseline)
    IdSorted,
    /// descending global zone area — deterministic per set, same interning as IdSorted
    AreaDesc,
    /// this cell's dominant zone first, rest id-sorted — best early-exit, breaks interning
    CellDominantFirst,
}

pub struct Csr {
    /// u16 per cell: high bit 0 = zone id (or NO_ZONE marker semantics left to
    /// the container), high bit 1 = index into the interned lists
    pub primary: Vec<u16>,
    pub list_offsets: Vec<u16>,
    pub list_ids: Vec<u16>,
    pub uniq_lists: usize,
}

impl Csr {
    pub fn bytes(&self) -> usize {
        self.primary.len() * 2 + self.list_offsets.len() * 2 + self.list_ids.len() * 2
    }
}

/// Build the interned CSR. `areas` (global zone area, any consistent unit) is
/// used by `AreaDesc`/`CellDominantFirst`.
pub fn intern_csr(grid: &CellGrid, order: Order, areas: &[f64]) -> Csr {
    let total = grid.ncols * grid.nrows;
    let mut primary = vec![0u16; total];
    let mut lists: Vec<Vec<u16>> = Vec::new();
    let mut index: HashMap<Vec<u16>, u16> = HashMap::new();
    let by_area = |v: &mut Vec<u16>| {
        v.sort_unstable_by(|&a, &b| areas[b as usize].partial_cmp(&areas[a as usize]).unwrap()
            .then(a.cmp(&b)));
    };
    for c in 0..total {
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
            let next = lists.len() as u16;
            let li = *index.entry(list.clone()).or_insert_with(|| { lists.push(list); next });
            primary[c] = 0x8000 | li;
        } else {
            // interior (single candidate) or no-ring cell: dominant zone
            let z = if set.len() == 1 { set[0] } else { grid.dominant[c] };
            primary[c] = if z == NO_ZONE { 0x7FFF } else { z };
        }
    }
    let mut list_offsets = Vec::with_capacity(lists.len() + 1);
    let mut list_ids = Vec::new();
    list_offsets.push(0u16);
    for l in &lists {
        list_ids.extend_from_slice(l);
        list_offsets.push(list_ids.len() as u16);
    }
    Csr { primary, list_offsets, list_ids, uniq_lists: lists.len() }
}

/// Approximate global area per feature (equirectangular shoelace with cos-lat
/// correction; exteriors minus holes, clamped ≥ 0). Ranking only.
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
    let midlat = ring.iter().map(|&(_, y)| y).sum::<f64>() / ring.len() as f64;
    let mut s = 0.0;
    for i in 0..ring.len() {
        let (x0, y0) = ring[i];
        let (x1, y1) = ring[(i + 1) % ring.len()];
        s += x0 * y1 - x1 * y0;
    }
    (s.abs() / 2.0) * midlat.to_radians().cos().abs()
}
