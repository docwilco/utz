//! Ring-geometry validation, which finds where assembled rings
//! self-cross, overlap themselves collinearly, self-touch, or collapse
//! entirely. It is shared by the `utz_dev_cli quant-clean` report and the
//! viewer's live problems panel (wasm.rs), so both agree on what
//! "problematic geometry" means.

use crate::clean::{self, CleanStats};
use crate::topo::Topology;
use crate::{Arc, Ring};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Cross,
    Overlap,
}

/// One located defect, in quantized grid units (see [`Bad::locs`]).
#[derive(Clone, Copy, Debug)]
pub struct Loc {
    /// The ring ordinal within the measured iteration.
    pub ring: usize,
    pub kind: Kind,
    pub x: f64,
    pub y: f64,
}

#[derive(Default, Clone)]
pub struct Bad {
    /// The number of non-adjacent segment pairs that properly cross.
    pub crossings: usize,
    /// The number of non-adjacent collinear segment pairs overlapping in
    /// more than a point.
    pub overlaps: usize,
    /// The number of non-adjacent segment pairs touching in exactly one
    /// point.
    pub touches: usize,
    /// The number of rings with fewer than 3 distinct vertices or zero
    /// area.
    pub degenerate: usize,
    pub verts: usize,
    /// The located crossing/overlap spots (touches are counted but not
    /// located).
    pub locs: Vec<Loc>,
}

/// Sweeps every ring and tallies the problems: self-crossings, collinear
/// overlaps, self-touches, and degenerate rings.
#[must_use]
pub fn measure(rings: impl Iterator<Item = Ring<i32>>) -> Bad {
    let mut bad = Bad::default();
    for (ring_index, coords) in rings.enumerate() {
        bad.verts += coords.len();
        if clean::ring_degenerate(&coords) {
            bad.degenerate += 1;
            continue;
        }
        ring_bad(ring_index, &coords, &mut bad);
    }
    bad
}

/// A surviving defect of one feature's geometry, in degrees. This is
/// what the viewer's problems panel lists.
#[derive(Clone, Copy, Debug)]
pub struct Problem {
    pub lon: f64,
    pub lat: f64,
    pub kind: Kind,
    /// The owning feature index.
    pub feat: usize,
}

/// Runs the encoder's quantize, clean, and drop-degenerate pipeline on
/// already-simplified arcs at `qbits`, then locates every surviving
/// crossing/overlap. A spot on a shared border is reported once per
/// owning ring (dedupe by coordinates for display).
#[must_use]
pub fn find_problems(
    topology: &Topology,
    arc_coords: &[Vec<(f64, f64)>],
    qbits: u32,
) -> Vec<Problem> {
    let qmax = crate::qmax_for(qbits);
    let mut clean_stats = CleanStats::default();
    let quantize = |&(lon, lat): &(f64, f64)| (crate::q_lon(lon, qmax), crate::q_lat(lat, qmax));
    let arcs_q: Vec<Arc<i32>> = arc_coords
        .iter()
        .map(|arc| {
            let mut quantized: Vec<(i32, i32)> = arc.iter().map(quantize).collect();
            let closed = arc.len() > 1 && arc.first() == arc.last();
            clean::clean_arc(&mut quantized, closed, &mut clean_stats);
            quantized
        })
        .collect();
    let (ring_refs, structure, arcs_q) = clean::drop_degenerate_rings(
        &topology.ring_refs,
        &topology.structure,
        arcs_q,
        &mut clean_stats,
    );

    let mut owner = vec![usize::MAX; ring_refs.len()];
    for (feature_index, feature) in structure.iter().enumerate() {
        for poly in feature {
            for &ring_index in poly {
                owner[ring_index] = feature_index;
            }
        }
    }
    let bad = measure(
        ring_refs
            .iter()
            .map(|refs| clean::ring_coords_q(refs, &arcs_q)),
    );
    bad.locs
        .iter()
        .map(|location| Problem {
            lon: crate::dq_lon(location.x, qmax),
            lat: crate::dq_lat(location.y, qmax),
            kind: location.kind,
            feat: owner[location.ring],
        })
        .collect()
}

/// Counts the non-adjacent segment pairs of one ring that intersect,
/// split by kind. The sweep runs over min-x-sorted segments in
/// O(n log n + pairs-in-x-overlap), which is fine at report scale.
/// Crossing/overlap spots land in `bad.locs`.
///
/// # Panics
/// Panics if a ring has more than `u32::MAX` segments.
pub fn ring_bad(ring_index: usize, ring: &[(i32, i32)], bad: &mut Bad) {
    let len = ring.len();
    if len < 4 {
        return;
    }
    let segment = |index: usize| (ring[index], ring[(index + 1) % len]);
    let min_x = |index: usize| {
        let (a, b) = segment(index);
        a.0.min(b.0)
    };
    let mut order: Vec<u32> = (0..u32::try_from(len).expect("segment count fits u32")).collect();
    order.sort_unstable_by_key(|&index| min_x(index as usize));
    for sweep_position in 0..len {
        let segment_a = order[sweep_position] as usize;
        let (p1, p2) = segment(segment_a);
        let (max_x, min_y, max_y) = (p1.0.max(p2.0), p1.1.min(p2.1), p1.1.max(p2.1));
        for &other in &order[sweep_position + 1..] {
            let segment_b = other as usize;
            let (q1, q2) = segment(segment_b);
            if q1.0.min(q2.0) > max_x {
                break;
            }
            if segment_a.abs_diff(segment_b) == 1 || segment_a.abs_diff(segment_b) == len - 1 {
                continue; // adjacent segments share a vertex by construction
            }
            if q1.1.max(q2.1) < min_y || q1.1.min(q2.1) > max_y {
                continue;
            }
            match seg_class((p1, p2), (q1, q2)) {
                Class::Cross => {
                    bad.crossings += 1;
                    let (x, y) = cross_point((p1, p2), (q1, q2));
                    bad.locs.push(Loc {
                        ring: ring_index,
                        kind: Kind::Cross,
                        x,
                        y,
                    });
                }
                Class::Overlap => {
                    bad.overlaps += 1;
                    // midpoint of the shared stretch: average the two middle
                    // endpoints along the sort order
                    let mut points = [p1, p2, q1, q2];
                    points.sort_unstable();
                    let (x, y) = (
                        f64::midpoint(f64::from(points[1].0), f64::from(points[2].0)),
                        f64::midpoint(f64::from(points[1].1), f64::from(points[2].1)),
                    );
                    bad.locs.push(Loc {
                        ring: ring_index,
                        kind: Kind::Overlap,
                        x,
                        y,
                    });
                }
                Class::Touch => bad.touches += 1,
                Class::None => {}
            }
        }
    }
}

/// Computes the intersection point of two properly crossing segments
/// (the denominator is nonzero exactly because they properly cross).
fn cross_point(
    (p1, p2): ((i32, i32), (i32, i32)),
    (q1, q2): ((i32, i32), (i32, i32)),
) -> (f64, f64) {
    let (dx, dy) = (f64::from(p2.0 - p1.0), f64::from(p2.1 - p1.1));
    let (ex, ey) = (f64::from(q2.0 - q1.0), f64::from(q2.1 - q1.1));
    let denominator = dx * ey - dy * ex;
    let t = (f64::from(q1.0 - p1.0) * ey - f64::from(q1.1 - p1.1) * ex) / denominator;
    (f64::from(p1.0) + t * dx, f64::from(p1.1) + t * dy)
}

enum Class {
    None,
    Touch,
    Cross,
    Overlap,
}

fn orient(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> i128 {
    (i128::from(b.0) - i128::from(a.0)) * (i128::from(c.1) - i128::from(a.1))
        - (i128::from(b.1) - i128::from(a.1)) * (i128::from(c.0) - i128::from(a.0))
}

fn sgn(value: i128) -> i8 {
    i8::from(value > 0) - i8::from(value < 0)
}

fn in_bbox(p: (i32, i32), a: (i32, i32), b: (i32, i32)) -> bool {
    p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

fn seg_class((p1, p2): ((i32, i32), (i32, i32)), (q1, q2): ((i32, i32), (i32, i32))) -> Class {
    let (o1, o2) = (sgn(orient(p1, p2, q1)), sgn(orient(p1, p2, q2)));
    let (o3, o4) = (sgn(orient(q1, q2, p1)), sgn(orient(q1, q2, p2)));
    if o1 == 0 && o2 == 0 && o3 == 0 && o4 == 0 {
        // collinear: project on the dominant axis and compare 1D ranges
        let flat = p1.0 == p2.0 && q1.0 == q2.0;
        let axis_value = |p: (i32, i32)| if flat { p.1 } else { p.0 };
        let (a0, a1) = (
            axis_value(p1).min(axis_value(p2)),
            axis_value(p1).max(axis_value(p2)),
        );
        let (b0, b1) = (
            axis_value(q1).min(axis_value(q2)),
            axis_value(q1).max(axis_value(q2)),
        );
        let (lo, hi) = (a0.max(b0), a1.min(b1));
        return match lo.cmp(&hi) {
            core::cmp::Ordering::Less => Class::Overlap,
            core::cmp::Ordering::Equal => Class::Touch,
            core::cmp::Ordering::Greater => Class::None,
        };
    }
    if o1 != o2 && o3 != o4 && o1 != 0 && o2 != 0 && o3 != 0 && o4 != 0 {
        return Class::Cross;
    }
    // an endpoint of one segment lying on the other = touch
    if (o1 == 0 && in_bbox(q1, p1, p2))
        || (o2 == 0 && in_bbox(q2, p1, p2))
        || (o3 == 0 && in_bbox(p1, q1, q2))
        || (o4 == 0 && in_bbox(p2, q1, q2))
    {
        return Class::Touch;
    }
    Class::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bowtie_ring_crosses_once() {
        // figure-8: segments (0,0)-(10,10) and (10,0)-(0,10) properly cross
        let ring = vec![(0, 0), (10, 10), (10, 0), (0, 10)];
        let bad = measure(vec![ring].into_iter());
        assert_eq!(bad.crossings, 1);
        assert_eq!(bad.locs.len(), 1);
        let location = bad.locs[0];
        assert_eq!((location.x, location.y), (5.0, 5.0));
    }

    #[test]
    fn square_ring_is_clean() {
        let ring = vec![(0, 0), (10, 0), (10, 10), (0, 10)];
        let bad = measure(vec![ring].into_iter());
        assert_eq!(
            (bad.crossings, bad.overlaps, bad.touches, bad.degenerate),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn find_problems_locates_bowtie_in_degrees() {
        // one feature, one ring stored as a single closed arc, bowtie shape
        // big enough to survive i16 snapping (~0.005° cells)
        let arc: Vec<(f64, f64)> = vec![(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)];
        let topology = Topology {
            arc_coords: vec![arc.clone()],
            ring_refs: vec![vec![0 << 1]],
            structure: vec![vec![vec![0]]],
        };
        let problems = find_problems(&topology, &topology.arc_coords, 16);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].feat, 0);
        assert!(matches!(problems[0].kind, Kind::Cross));
        assert!(
            (problems[0].lon - 0.5).abs() < 0.01 && (problems[0].lat - 0.5).abs() < 0.01,
            "{:?}",
            problems[0]
        );
    }
}
