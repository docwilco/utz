//! Post-quantization cleanup. Snapping arc vertices to a coarse grid
//! (especially i16, ~611 m cells) collapses nearby vertices and folds thin
//! features onto themselves: consecutive duplicates, zero-area spurs where
//! the path reverses over itself ("spikes"), and rings whose area vanishes
//! entirely. Left in, the spurs self-overlap and flip the runtime's even-odd
//! PIP parity inside the fold.
//!
//! Every fix runs on the shared arcs (or drops whole rings) — never on one
//! polygon in isolation — so neighbouring zones stay stitched by
//! construction; cleaning a border cleans it identically for both owners.

#[derive(Clone, Copy, Default, Debug)]
pub struct CleanStats {
    /// consecutive duplicate vertices removed
    pub dups: u32,
    /// zero-area spur vertices removed (path reverses along the same line)
    pub spikes: u32,
    /// collinear pass-through vertices removed (no geometry change)
    pub collinear: u32,
    /// degenerate rings dropped (fewer than 3 distinct vertices or area 0)
    pub rings_dropped: u32,
    /// polygons dropped because their exterior ring degenerated
    pub polys_dropped: u32,
    /// arcs left unreferenced by ring drops (removed, ids compacted)
    pub arcs_dropped: u32,
}

enum Kind {
    Spike,
    Collinear,
    Keep,
}

/// How the path bends at `q` between `p` and `r` (all distinct from `q`).
fn classify(p: (i32, i32), q: (i32, i32), r: (i32, i32)) -> Kind {
    let (ax, ay) = ((q.0 - p.0) as i64, (q.1 - p.1) as i64);
    let (bx, by) = ((r.0 - q.0) as i64, (r.1 - q.1) as i64);
    if ax * by != ay * bx {
        return Kind::Keep;
    }
    if ax * bx + ay * by < 0 { Kind::Spike } else { Kind::Collinear }
}

/// Remove quantization artifacts from one quantized arc, in place.
///
/// An interior vertex goes when it duplicates its predecessor, when the path
/// reverses over it along the same line (zero-area spike; iterated, so
/// multi-vertex spurs unwind fully), or when it lies collinearly between its
/// neighbours. `closed` arcs (cut-free rings, stored with first == last) are
/// cleaned cyclically so artifacts at the arbitrary start vertex are caught
/// too; open arcs never lose their endpoints — those are junctions shared
/// with other arcs.
pub fn clean_arc(a: &mut Vec<(i32, i32)>, closed: bool, st: &mut CleanStats) {
    if closed {
        if a.len() > 1 && a.first() == a.last() {
            a.pop();
        }
        clean_cyclic(a, st);
        if a.len() > 1 {
            let f = a[0];
            a.push(f);
        }
    } else {
        clean_open(a, st);
    }
}

fn clean_open(a: &mut Vec<(i32, i32)>, st: &mut CleanStats) {
    let mut i = 1;
    while i < a.len() {
        if a[i] == a[i - 1] {
            a.remove(i);
            st.dups += 1;
            i = i.saturating_sub(1).max(1);
            continue;
        }
        if i + 1 == a.len() {
            break;
        }
        if a[i] == a[i + 1] {
            a.remove(i);
            st.dups += 1;
            i = i.saturating_sub(1).max(1);
            continue;
        }
        match classify(a[i - 1], a[i], a[i + 1]) {
            Kind::Spike => {
                a.remove(i);
                st.spikes += 1;
                i = i.saturating_sub(1).max(1);
            }
            Kind::Collinear => {
                a.remove(i);
                st.collinear += 1;
                i = i.saturating_sub(1).max(1);
            }
            Kind::Keep => i += 1,
        }
    }
}

fn clean_cyclic(a: &mut Vec<(i32, i32)>, st: &mut CleanStats) {
    loop {
        let mut changed = false;
        let mut i = 0;
        while a.len() >= 3 && i < a.len() {
            let n = a.len();
            let (p, q, r) = (a[(i + n - 1) % n], a[i], a[(i + 1) % n]);
            if q == p || q == r {
                a.remove(i);
                st.dups += 1;
                changed = true;
                continue;
            }
            match classify(p, q, r) {
                Kind::Spike => {
                    a.remove(i);
                    st.spikes += 1;
                    changed = true;
                }
                Kind::Collinear => {
                    a.remove(i);
                    st.collinear += 1;
                    changed = true;
                }
                Kind::Keep => i += 1,
            }
        }
        if !changed || a.len() < 3 {
            break;
        }
    }
    if a.len() == 2 && a[0] == a[1] {
        a.pop();
        st.dups += 1;
    }
}

/// Assemble one ring's quantized coords from its signed arc refs — the
/// integer twin of `Topology::reconstruct`'s ring assembly.
pub fn ring_coords_q(refs: &[u32], arcs: &[Vec<(i32, i32)>]) -> Vec<(i32, i32)> {
    let mut c: Vec<(i32, i32)> = Vec::new();
    for &r in refs {
        let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
        let mut a = arcs[id].clone();
        if rev {
            a.reverse();
        }
        if c.last() == a.first() {
            a.remove(0);
        }
        c.extend(a);
    }
    if c.len() > 1 && c.last() == c.first() {
        c.pop();
    }
    c
}

/// Ring collapsed under quantization: fewer than 3 vertices, or shoelace
/// area exactly 0 (spur-only remnant). Exact in i128 for all qbits.
pub fn ring_degenerate(c: &[(i32, i32)]) -> bool {
    if c.len() < 3 {
        return true;
    }
    let mut a2: i128 = 0;
    for i in 0..c.len() {
        let (p, q) = (c[i], c[(i + 1) % c.len()]);
        a2 += p.0 as i128 * q.1 as i128 - q.0 as i128 * p.1 as i128;
    }
    a2 == 0
}

/// Drop rings that quantization collapsed to zero area: a degenerate hole
/// vanishes alone, a degenerate exterior takes its holes with it. Arcs no
/// surviving ring references are removed and arc ids compacted. Returns the
/// filtered `(ring_refs, structure, arcs)` mirroring `Topology`'s fields.
/// Dropping a zero-area ring can't open a crack with a neighbour — there was
/// no area to disagree about.
pub fn drop_degenerate_rings(
    ring_refs: &[Vec<u32>],
    structure: &[Vec<Vec<usize>>],
    arcs: Vec<Vec<(i32, i32)>>,
    st: &mut CleanStats,
) -> (Vec<Vec<u32>>, Vec<Vec<Vec<usize>>>, Vec<Vec<(i32, i32)>>) {
    let ring_ok: Vec<bool> = ring_refs
        .iter()
        .map(|refs| !ring_degenerate(&ring_coords_q(refs, &arcs)))
        .collect();

    let mut new_refs: Vec<Vec<u32>> = Vec::new();
    let mut new_structure: Vec<Vec<Vec<usize>>> = Vec::with_capacity(structure.len());
    for f in structure {
        let mut fp: Vec<Vec<usize>> = Vec::new();
        for poly in f {
            match poly.first() {
                Some(&ext) if ring_ok[ext] => {}
                _ => {
                    st.polys_dropped += 1;
                    st.rings_dropped += poly.len() as u32;
                    continue;
                }
            }
            let mut pr = Vec::with_capacity(poly.len());
            for (k, &ri) in poly.iter().enumerate() {
                if k > 0 && !ring_ok[ri] {
                    st.rings_dropped += 1;
                    continue;
                }
                pr.push(new_refs.len());
                new_refs.push(ring_refs[ri].clone());
            }
            fp.push(pr);
        }
        new_structure.push(fp);
    }

    // compact arc ids to the surviving rings
    let mut used = vec![false; arcs.len()];
    for refs in &new_refs {
        for &r in refs {
            used[(r >> 1) as usize] = true;
        }
    }
    let mut remap = vec![u32::MAX; arcs.len()];
    let mut new_arcs = Vec::with_capacity(arcs.len());
    for (i, a) in arcs.into_iter().enumerate() {
        if used[i] {
            remap[i] = new_arcs.len() as u32;
            new_arcs.push(a);
        } else {
            st.arcs_dropped += 1;
        }
    }
    for refs in &mut new_refs {
        for r in refs.iter_mut() {
            *r = (remap[(*r >> 1) as usize] << 1) | (*r & 1);
        }
    }
    (new_refs, new_structure, new_arcs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> CleanStats {
        CleanStats::default()
    }

    #[test]
    fn open_arc_keeps_endpoints_and_kills_spike() {
        // A-B-A retrace in the middle of an arc
        let mut a = vec![(0, 0), (5, 0), (9, 0), (5, 0), (5, 5)];
        let mut st = stats();
        clean_arc(&mut a, false, &mut st);
        assert_eq!(a, vec![(0, 0), (5, 0), (5, 5)]);
        assert!(st.spikes >= 1);
    }

    #[test]
    fn open_arc_multi_vertex_spur_unwinds() {
        // spur wanders out two vertices and retraces exactly
        let mut a = vec![(0, 0), (4, 0), (4, 3), (4, 9), (4, 3), (4, 0), (8, 0)];
        let mut st = stats();
        clean_arc(&mut a, false, &mut st);
        assert_eq!(a, vec![(0, 0), (8, 0)]);
    }

    #[test]
    fn open_arc_partial_retrace_spur() {
        // reverses along the same line but not onto an existing vertex
        let mut a = vec![(0, 0), (10, 0), (3, 0), (3, 4)];
        let mut st = stats();
        clean_arc(&mut a, false, &mut st);
        assert_eq!(a, vec![(0, 0), (3, 0), (3, 4)]);
        assert_eq!(st.spikes, 1);
    }

    #[test]
    fn open_arc_collinear_and_dups() {
        let mut a = vec![(0, 0), (0, 0), (2, 0), (5, 0), (5, 0), (9, 0)];
        let mut st = stats();
        clean_arc(&mut a, false, &mut st);
        assert_eq!(a, vec![(0, 0), (9, 0)]);
        assert_eq!(st.dups, 2);
        assert_eq!(st.collinear, 2);
    }

    #[test]
    fn closed_arc_spike_at_start_vertex() {
        // ring stored first == last, spur sits exactly on the start vertex —
        // the open-arc pass can't touch it, the cyclic pass must
        let mut a = vec![(0, 0), (5, -5), (0, 0), (10, 0), (10, 10), (0, 10), (0, 0)];
        let mut st = stats();
        clean_arc(&mut a, true, &mut st);
        let n = a.len();
        assert_eq!(a[0], a[n - 1]);
        let interior: Vec<_> = a[..n - 1].to_vec();
        assert_eq!(interior.len(), 4);
        assert!(!interior.contains(&(5, -5)));
    }

    #[test]
    fn closed_arc_collapses_to_degenerate() {
        // entire ring snaps onto one line; whatever remnant survives must
        // read as a degenerate ring so the ring-level drop removes it
        let mut a = vec![(0, 0), (5, 0), (9, 0), (5, 0), (0, 0)];
        let mut st = stats();
        clean_arc(&mut a, true, &mut st);
        assert!(ring_degenerate(&ring_coords_q(&[0 << 1], &[a.clone()])), "{a:?}");
    }

    #[test]
    fn degenerate_ring_detection() {
        assert!(ring_degenerate(&[(0, 0), (5, 0)]));
        assert!(ring_degenerate(&[(0, 0), (5, 0), (9, 0)])); // zero area
        assert!(!ring_degenerate(&[(0, 0), (5, 0), (5, 5)]));
    }

    #[test]
    fn drop_degenerate_hole_keeps_poly_and_compacts_arcs() {
        // poly 0: square exterior (arc 0, closed) + zero-area hole (arc 1)
        let arcs = vec![
            vec![(0, 0), (10, 0), (10, 10), (0, 10), (0, 0)],
            vec![(2, 2), (6, 2), (2, 2)],
        ];
        let ring_refs = vec![vec![0u32 << 1], vec![1u32 << 1]];
        let structure = vec![vec![vec![0usize, 1]]];
        let mut st = stats();
        let (refs, s, arcs) = drop_degenerate_rings(&ring_refs, &structure, arcs, &mut st);
        assert_eq!(s, vec![vec![vec![0usize]]]);
        assert_eq!(refs.len(), 1);
        assert_eq!(arcs.len(), 1);
        assert_eq!(st.rings_dropped, 1);
        assert_eq!(st.arcs_dropped, 1);
        assert_eq!(st.polys_dropped, 0);
    }

    #[test]
    fn drop_degenerate_exterior_takes_holes() {
        let arcs = vec![
            vec![(0, 0), (10, 0), (0, 0)],                    // flat exterior
            vec![(2, 2), (6, 2), (6, 6), (2, 6), (2, 2)],     // healthy hole
        ];
        let ring_refs = vec![vec![0u32 << 1], vec![1u32 << 1]];
        let structure = vec![vec![vec![0usize, 1]]];
        let mut st = stats();
        let (refs, s, arcs) = drop_degenerate_rings(&ring_refs, &structure, arcs, &mut st);
        assert_eq!(s, vec![Vec::<Vec<usize>>::new()]);
        assert!(refs.is_empty());
        assert!(arcs.is_empty());
        assert_eq!(st.polys_dropped, 1);
        assert_eq!(st.rings_dropped, 2);
        assert_eq!(st.arcs_dropped, 2);
    }
}
