//! Post-quantization cleanup of the shared arcs. Snapping arc vertices to a
//! coarse grid (especially i16, ~611 m cells) collapses nearby vertices and
//! folds thin features onto themselves, leaving consecutive duplicates,
//! zero-area spurs where the path reverses over itself ("spikes"), and
//! rings whose area vanishes entirely. Left in, the spurs self-overlap and
//! flip the runtime's even-odd PIP parity inside the fold.
//!
//! Every fix runs on the shared arcs (or drops whole rings), never on one
//! polygon in isolation, so neighbouring zones stay stitched by
//! construction; cleaning a border cleans it identically for both owners.

use crate::{Arc, Ring};

#[derive(Clone, Copy, Default, Debug)]
pub struct CleanStats {
    /// The number of consecutive duplicate vertices removed.
    pub dups: u32,
    /// The number of zero-area spur vertices removed (the path reverses
    /// along the same line).
    pub spikes: u32,
    /// The number of collinear pass-through vertices removed (no geometry
    /// change).
    pub collinear: u32,
    /// The number of degenerate rings dropped (fewer than 3 distinct
    /// vertices, or area 0).
    pub rings_dropped: u32,
    /// The number of polygons dropped because their exterior ring
    /// degenerated.
    pub polys_dropped: u32,
    /// The number of arcs left unreferenced by ring drops; they are removed
    /// and the ids compacted.
    pub arcs_dropped: u32,
}

enum Kind {
    Spike,
    Collinear,
    Keep,
}

/// Classifies how the path bends at `q` between `p` and `r` (both distinct
/// from `q`).
fn classify(p: (i32, i32), q: (i32, i32), r: (i32, i32)) -> Kind {
    let (ax, ay) = (i64::from(q.0 - p.0), i64::from(q.1 - p.1));
    let (bx, by) = (i64::from(r.0 - q.0), i64::from(r.1 - q.1));
    if ax * by != ay * bx {
        return Kind::Keep;
    }
    if ax * bx + ay * by < 0 {
        Kind::Spike
    } else {
        Kind::Collinear
    }
}

/// Removes quantization artifacts from one quantized arc, in place.
///
/// An interior vertex goes when it duplicates its predecessor, when the path
/// reverses over it along the same line (a zero-area spike; the pass
/// iterates, so multi-vertex spurs unwind fully), or when it lies
/// collinearly between its
/// neighbours. `closed` arcs (cut-free rings, stored with first == last) are
/// cleaned cyclically so artifacts at the arbitrary start vertex are caught
/// too; open arcs never lose their endpoints: those are junctions shared
/// with other arcs.
pub fn clean_arc(arc: &mut Arc<i32>, closed: bool, stats: &mut CleanStats) {
    if closed {
        if arc.len() > 1 && arc.first() == arc.last() {
            arc.pop();
        }
        clean_cyclic(arc, stats);
        if arc.len() > 1 {
            let first = arc[0];
            arc.push(first);
        }
    } else {
        clean_open(arc, stats);
    }
}

fn clean_open(arc: &mut Arc<i32>, stats: &mut CleanStats) {
    let mut i = 1;
    while i < arc.len() {
        if arc[i] == arc[i - 1] {
            arc.remove(i);
            stats.dups += 1;
            i = i.saturating_sub(1).max(1);
            continue;
        }
        if i + 1 == arc.len() {
            break;
        }
        if arc[i] == arc[i + 1] {
            arc.remove(i);
            stats.dups += 1;
            i = i.saturating_sub(1).max(1);
            continue;
        }
        match classify(arc[i - 1], arc[i], arc[i + 1]) {
            Kind::Spike => {
                arc.remove(i);
                stats.spikes += 1;
                i = i.saturating_sub(1).max(1);
            }
            Kind::Collinear => {
                arc.remove(i);
                stats.collinear += 1;
                i = i.saturating_sub(1).max(1);
            }
            Kind::Keep => i += 1,
        }
    }
}

fn clean_cyclic(arc: &mut Arc<i32>, stats: &mut CleanStats) {
    loop {
        let mut changed = false;
        let mut index = 0;
        while arc.len() >= 3 && index < arc.len() {
            let len = arc.len();
            let (previous, current, next) = (
                arc[(index + len - 1) % len],
                arc[index],
                arc[(index + 1) % len],
            );
            if current == previous || current == next {
                arc.remove(index);
                stats.dups += 1;
                changed = true;
                continue;
            }
            match classify(previous, current, next) {
                Kind::Spike => {
                    arc.remove(index);
                    stats.spikes += 1;
                    changed = true;
                }
                Kind::Collinear => {
                    arc.remove(index);
                    stats.collinear += 1;
                    changed = true;
                }
                Kind::Keep => index += 1,
            }
        }
        if !changed || arc.len() < 3 {
            break;
        }
    }
    if arc.len() == 2 && arc[0] == arc[1] {
        arc.pop();
        stats.dups += 1;
    }
}

/// Assembles one ring's quantized coords from its signed arc refs, the
/// integer twin of `Topology::reconstruct()`'s ring assembly.
#[must_use]
pub fn ring_coords_q(refs: &[u32], arcs: &[Arc<i32>]) -> Ring<i32> {
    let mut coords: Ring<i32> = Vec::new();
    for &arc_ref in refs {
        let (id, reversed) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
        let mut arc = arcs[id].clone();
        if reversed {
            arc.reverse();
        }
        if coords.last() == arc.first() {
            arc.remove(0);
        }
        coords.extend(arc);
    }
    if coords.len() > 1 && coords.last() == coords.first() {
        coords.pop();
    }
    coords
}

/// Reports whether the ring collapsed under quantization: fewer than 3
/// vertices remain, or the shoelace area is exactly 0 with no proper
/// self-crossing. The crossing exemption
/// matters: a bowtie with equal opposite lobes has signed area 0 yet still
/// covers both lobes under the runtime's even-odd rule. Dropping it would
/// lose real coverage. The computation is exact in i128 for all qbits.
#[must_use]
pub fn ring_degenerate(coords: &[(i32, i32)]) -> bool {
    if coords.len() < 3 {
        return true;
    }
    let mut doubled_area: i128 = 0;
    for i in 0..coords.len() {
        let (p, q) = (coords[i], coords[(i + 1) % coords.len()]);
        doubled_area += i128::from(p.0) * i128::from(q.1) - i128::from(q.0) * i128::from(p.1);
    }
    doubled_area == 0 && !has_proper_cross(coords)
}

/// Reports whether any pair of non-adjacent ring segments properly crosses
/// (their interiors intersect). The scan is O(n²), but it is only reached
/// for zero-area rings, which quantization keeps tiny.
fn has_proper_cross(coords: &[(i32, i32)]) -> bool {
    let len = coords.len();
    let orient = |a: (i32, i32), b: (i32, i32), p: (i32, i32)| -> i8 {
        let value = (i128::from(b.0) - i128::from(a.0)) * (i128::from(p.1) - i128::from(a.1))
            - (i128::from(b.1) - i128::from(a.1)) * (i128::from(p.0) - i128::from(a.0));
        i8::from(value > 0) - i8::from(value < 0)
    };
    for i in 0..len {
        let (p1, p2) = (coords[i], coords[(i + 1) % len]);
        for j in i + 2..len {
            if i == 0 && j == len - 1 {
                continue; // adjacent around the wrap
            }
            let (q1, q2) = (coords[j], coords[(j + 1) % len]);
            let (o1, o2) = (orient(p1, p2, q1), orient(p1, p2, q2));
            let (o3, o4) = (orient(q1, q2, p1), orient(q1, q2, p2));
            if o1 != o2 && o3 != o4 && o1 != 0 && o2 != 0 && o3 != 0 && o4 != 0 {
                return true;
            }
        }
    }
    false
}

/// The `(ring_refs, structure, arcs)` triple mirroring `Topology`'s fields,
/// with arcs quantized to integer coordinates.
pub type CleanedTopo = (Vec<Vec<u32>>, Vec<Vec<Vec<usize>>>, Vec<Arc<i32>>);

/// Drops rings that quantization collapsed to zero area: a degenerate hole
/// vanishes alone, and a degenerate exterior takes its holes with it. Arcs
/// that no surviving ring references are removed, and their ids are
/// compacted. Returns the
/// filtered `(ring_refs, structure, arcs)` mirroring `Topology`'s fields.
/// Dropping a zero-area ring can't open a crack with a neighbour: there was
/// no area to disagree about.
///
/// # Panics
/// Panics if a polygon has more than `u32::MAX` rings.
pub fn drop_degenerate_rings(
    ring_refs: &[Vec<u32>],
    structure: &[Vec<Vec<usize>>],
    arcs: Vec<Arc<i32>>,
    stats: &mut CleanStats,
) -> CleanedTopo {
    let ring_ok: Vec<bool> = ring_refs
        .iter()
        .map(|refs| !ring_degenerate(&ring_coords_q(refs, &arcs)))
        .collect();

    let mut new_refs: Vec<Vec<u32>> = Vec::new();
    let mut new_structure: Vec<Vec<Vec<usize>>> = Vec::with_capacity(structure.len());
    for feature in structure {
        let mut feature_polys: Vec<Vec<usize>> = Vec::new();
        for poly in feature {
            match poly.first() {
                Some(&exterior) if ring_ok[exterior] => {}
                _ => {
                    stats.polys_dropped += 1;
                    stats.rings_dropped += u32::try_from(poly.len()).expect("ring count fits u32");
                    continue;
                }
            }
            let mut poly_rings = Vec::with_capacity(poly.len());
            for (k, &ring_index) in poly.iter().enumerate() {
                if k > 0 && !ring_ok[ring_index] {
                    stats.rings_dropped += 1;
                    continue;
                }
                poly_rings.push(new_refs.len());
                new_refs.push(ring_refs[ring_index].clone());
            }
            feature_polys.push(poly_rings);
        }
        new_structure.push(feature_polys);
    }

    // compact arc ids to the surviving rings
    let mut used = vec![false; arcs.len()];
    for refs in &new_refs {
        for &arc_ref in refs {
            used[(arc_ref >> 1) as usize] = true;
        }
    }
    let mut remap = vec![u32::MAX; arcs.len()];
    let mut new_arcs = Vec::with_capacity(arcs.len());
    for (i, arc) in arcs.into_iter().enumerate() {
        if used[i] {
            remap[i] = u32::try_from(new_arcs.len()).expect("arc count fits u32");
            new_arcs.push(arc);
        } else {
            stats.arcs_dropped += 1;
        }
    }
    for refs in &mut new_refs {
        for arc_ref in refs.iter_mut() {
            *arc_ref = (remap[(*arc_ref >> 1) as usize] << 1) | (*arc_ref & 1);
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
        let mut arc = vec![(0, 0), (5, 0), (9, 0), (5, 0), (5, 5)];
        let mut stats = stats();
        clean_arc(&mut arc, false, &mut stats);
        assert_eq!(arc, vec![(0, 0), (5, 0), (5, 5)]);
        assert!(stats.spikes >= 1);
    }

    #[test]
    fn open_arc_multi_vertex_spur_unwinds() {
        // spur wanders out two vertices and retraces exactly
        let mut arc = vec![(0, 0), (4, 0), (4, 3), (4, 9), (4, 3), (4, 0), (8, 0)];
        let mut stats = stats();
        clean_arc(&mut arc, false, &mut stats);
        assert_eq!(arc, vec![(0, 0), (8, 0)]);
    }

    #[test]
    fn open_arc_partial_retrace_spur() {
        // reverses along the same line but not onto an existing vertex
        let mut arc = vec![(0, 0), (10, 0), (3, 0), (3, 4)];
        let mut stats = stats();
        clean_arc(&mut arc, false, &mut stats);
        assert_eq!(arc, vec![(0, 0), (3, 0), (3, 4)]);
        assert_eq!(stats.spikes, 1);
    }

    #[test]
    fn open_arc_collinear_and_dups() {
        let mut arc = vec![(0, 0), (0, 0), (2, 0), (5, 0), (5, 0), (9, 0)];
        let mut stats = stats();
        clean_arc(&mut arc, false, &mut stats);
        assert_eq!(arc, vec![(0, 0), (9, 0)]);
        assert_eq!(stats.dups, 2);
        assert_eq!(stats.collinear, 2);
    }

    #[test]
    fn closed_arc_spike_at_start_vertex() {
        // ring stored first == last, spur sits exactly on the start vertex —
        // the open-arc pass can't touch it, the cyclic pass must
        let mut arc = vec![(0, 0), (5, -5), (0, 0), (10, 0), (10, 10), (0, 10), (0, 0)];
        let mut stats = stats();
        clean_arc(&mut arc, true, &mut stats);
        let len = arc.len();
        assert_eq!(arc[0], arc[len - 1]);
        let interior: Vec<_> = arc[..len - 1].to_vec();
        assert_eq!(interior.len(), 4);
        assert!(!interior.contains(&(5, -5)));
    }

    #[test]
    fn closed_arc_collapses_to_degenerate() {
        // entire ring snaps onto one line; whatever remnant survives must
        // read as a degenerate ring so the ring-level drop removes it
        let mut arc = vec![(0, 0), (5, 0), (9, 0), (5, 0), (0, 0)];
        let mut stats = stats();
        clean_arc(&mut arc, true, &mut stats);
        assert!(
            ring_degenerate(&ring_coords_q(&[0 << 1], &[arc.clone()])),
            "{arc:?}"
        );
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
        let mut stats = stats();
        let (refs, s, arcs) = drop_degenerate_rings(&ring_refs, &structure, arcs, &mut stats);
        assert_eq!(s, vec![vec![vec![0usize]]]);
        assert_eq!(refs.len(), 1);
        assert_eq!(arcs.len(), 1);
        assert_eq!(stats.rings_dropped, 1);
        assert_eq!(stats.arcs_dropped, 1);
        assert_eq!(stats.polys_dropped, 0);
    }

    #[test]
    fn drop_degenerate_exterior_takes_holes() {
        let arcs = vec![
            vec![(0, 0), (10, 0), (0, 0)],                // flat exterior
            vec![(2, 2), (6, 2), (6, 6), (2, 6), (2, 2)], // healthy hole
        ];
        let ring_refs = vec![vec![0u32 << 1], vec![1u32 << 1]];
        let structure = vec![vec![vec![0usize, 1]]];
        let mut stats = stats();
        let (refs, s, arcs) = drop_degenerate_rings(&ring_refs, &structure, arcs, &mut stats);
        assert_eq!(s, vec![Vec::<Vec<usize>>::new()]);
        assert!(refs.is_empty());
        assert!(arcs.is_empty());
        assert_eq!(stats.polys_dropped, 1);
        assert_eq!(stats.rings_dropped, 2);
        assert_eq!(stats.arcs_dropped, 2);
    }
}
