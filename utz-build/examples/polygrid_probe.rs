//! Would a poly-granular grid replace the per-poly bboxes? (PLAN §10)
//!
//! Rebuilds the grid from a codec-*none* container's geometry twice with the
//! real builder (`grid::build` + `intern_csr`): once per feature (today's
//! format) and once with each polygon exploded into its own pseudo-feature
//! (the "purely grid" design — border-cell lists reference polys directly,
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
//!     cargo run --release -p utz-build --example polygrid_probe -- <none.utz>...

use utz::format::{self, fixed_bytes, read_fixed, read_u16, read_u32, read_varint, unzigzag};
use utz_build::grid::{self, Order};
use utz_build::Feat;

/// Decode one arc (forward orientation) into (i32, i32) coords.
fn arc_coords(p: &[u8], h: &format::Header, id: usize) -> Vec<(i32, i32)> {
    let fb = fixed_bytes(h.quant_bits);
    let mut pos = h.arc_data + read_u32(p, h.arc_offsets + id * 4) as usize;
    let (vcount, p2) = read_varint(p, pos);
    pos = p2;
    let mut coords = Vec::with_capacity(vcount as usize);
    if h.geom == 1 {
        for _ in 0..vcount {
            coords.push((read_fixed(p, pos, h.quant_bits), read_fixed(p, pos + fb, h.quant_bits)));
            pos += 2 * fb;
        }
        return coords;
    }
    let mut x = i64::from(read_fixed(p, pos, h.quant_bits));
    let mut y = i64::from(read_fixed(p, pos + fb, h.quant_bits));
    pos += 2 * fb;
    coords.push((x as i32, y as i32));
    for _ in 1..vcount {
        let (dx, p3) = read_varint(p, pos);
        let (dy, p4) = read_varint(p, p3);
        pos = p4;
        x += unzigzag(dx);
        y += unzigzag(dy);
        coords.push((x as i32, y as i32));
    }
    coords
}

/// Container → per-feature dequantized geometry (same dq as the encoder).
fn load_feats(bytes: &[u8]) -> (format::Header, Vec<Feat>) {
    let (codec, _, start) = format::outer(bytes).expect("not a utz container");
    assert_eq!(codec, 0, "need a codec-none container");
    let p = &bytes[start..];
    let h = format::parse(p).unwrap();
    assert!(h.geom <= 1, "arc-store containers only (geom 0/1)");
    let qmax = ((1u64 << (h.quant_bits - 1)) - 1) as f64;
    let dq = |v: i32, half: f64| f64::from(v) / qmax * half;
    let mut feats: Vec<Feat> = (0..h.n_features)
        .map(|_| Feat { offset: 0.0, tzid: None, polys: Vec::new() })
        .collect();
    let fb = fixed_bytes(h.quant_bits);
    for pid in 0..h.eager_polys as usize {
        let fi = read_u16(p, h.parent + pid * 2) as usize;
        let mut pos = h.ring_data + read_u32(p, h.poly_offsets + pid * 4) as usize;
        pos += 4 * fb; // per-poly bbox (v5)
        let nrings = read_u16(p, pos);
        pos += 2;
        let mut rings = Vec::with_capacity(nrings as usize);
        for _ in 0..nrings {
            let (nrefs, mut p2) = read_varint(p, pos);
            let mut ring: Vec<(f64, f64)> = Vec::new();
            for _ in 0..nrefs {
                let (r, p3) = read_varint(p, p2);
                p2 = p3;
                let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
                let mut c = arc_coords(p, &h, id);
                if rev {
                    c.reverse();
                }
                ring.extend(c.iter().map(|&(x, y)| (dq(x, 180.0), dq(y, 90.0))));
            }
            pos = p2;
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            rings.push(ring);
        }
        feats[fi].polys.push(rings);
    }
    (h, feats)
}

struct Stats {
    border_cells: usize,
    uniq_lists: usize,
    list_ids: usize,
    csr_bytes: usize,
    avg_list: f64,
    max_list: usize,
}

fn measure(feats: &[Feat], deg: f64) -> (grid::CellGrid, Stats) {
    let g = grid::build(feats, deg, 8);
    let areas = grid::feat_areas(feats);
    let csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);
    let border: Vec<&Vec<u16>> = g.sets.iter().filter(|s| s.len() > 1).collect();
    let s = Stats {
        border_cells: border.len(),
        uniq_lists: csr.uniq_lists,
        list_ids: csr.list_ids.len(),
        csr_bytes: csr.bytes(),
        avg_list: border.iter().map(|s| s.len()).sum::<usize>() as f64 / border.len().max(1) as f64,
        max_list: border.iter().map(|s| s.len()).max().unwrap_or(0),
    };
    (g, s)
}

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let (h, feats) = load_feats(&bytes);
        let deg = f64::from(h.grid_deg);
        let fb = fixed_bytes(h.quant_bits);
        let npolys: usize = feats.iter().map(|f| f.polys.len()).sum();
        let polys_per_feat: Vec<usize> = feats.iter().map(|f| f.polys.len()).collect();

        // explode: one pseudo-feature per polygon, remember the parent
        let mut poly_feats = Vec::with_capacity(npolys);
        let mut parent = Vec::with_capacity(npolys);
        for (fi, f) in feats.iter().enumerate() {
            for poly in &f.polys {
                poly_feats.push(Feat { offset: 0.0, tzid: None, polys: vec![poly.clone()] });
                parent.push(fi as u16);
            }
        }

        let (gf, sf) = measure(&feats, deg);
        let (gp, sp) = measure(&poly_feats, deg);

        // pruning waste today: in each border cell, candidate features drag
        // in ALL their polys (bbox-checked + ref-lists parsed); the poly
        // grid would visit only the polys whose rings touch the cell
        let (mut polys_today, mut polys_grid) = (0usize, 0usize);
        for (cf, cp) in gf.sets.iter().zip(gp.sets.iter()) {
            if cf.len() > 1 {
                polys_today += cf.iter().map(|&f| polys_per_feat[f as usize]).sum::<usize>();
                polys_grid += cp.iter().filter(|&&pi| {
                    // count only polys of candidate features (same PIP set)
                    cf.contains(&parent[pi as usize])
                }).count();
            }
        }

        // net size: grid growth − dropped bboxes + poly→feature id table
        // (ring records become directly addressable via the existing
        // feat_offsets-style table reshaped per poly; u16 parent id per poly)
        let bbox_bytes = npolys * 4 * fb;
        let parent_bytes = npolys * 2;
        let delta = sp.csr_bytes as isize - sf.csr_bytes as isize - bbox_bytes as isize
            + parent_bytes as isize;

        let name = std::path::Path::new(&path).file_stem().unwrap().to_string_lossy().into_owned();
        println!("== {name}: {} feats, {npolys} polys, {deg:.3}° grid ==", feats.len());
        println!("  border cells        feature-grid {:6}   poly-grid {:6}", sf.border_cells, sp.border_cells);
        println!("  uniq lists (cap 32767) {:9}          {:9}", sf.uniq_lists, sp.uniq_lists);
        println!("  list_ids (cap 65535)   {:9}          {:9}", sf.list_ids, sp.list_ids);
        println!("  csr bytes              {:9}          {:9}", sf.csr_bytes, sp.csr_bytes);
        println!("  avg/max list           {:5.2}/{:3}          {:5.2}/{:3}", sf.avg_list, sf.max_list, sp.avg_list, sp.max_list);
        println!(
            "  polys per border lookup: {:.1} today (bbox-pruned) vs {:.1} poly-grid ({:.1}x fewer)",
            polys_today as f64 / sf.border_cells.max(1) as f64,
            polys_grid as f64 / sf.border_cells.max(1) as f64,
            polys_today as f64 / polys_grid.max(1) as f64
        );
        println!(
            "  net size: csr {:+} − bboxes {} + parents {} = {:+} bytes",
            sp.csr_bytes as isize - sf.csr_bytes as isize, bbox_bytes, parent_bytes, delta
        );

        // grid-only-exact scaling: how fast do border cells shrink?
        print!("  grid-only scaling (feature grid): ");
        for d in [deg, deg / 2.0, deg / 4.0] {
            let (_, s) = measure(&feats, d);
            print!("{d:.3}°: {} border cells / {} KiB csr;  ", s.border_cells, s.csr_bytes / 1024);
        }
        println!();
    }
}
