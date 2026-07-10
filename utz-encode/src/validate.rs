//! Ring-geometry validation: find where assembled rings self-cross, overlap
//! themselves collinearly, self-touch, or collapse entirely. Shared by the
//! `utz-build quant-clean` report and the viewer's live problems panel
//! (wasm.rs), so both agree on what "problematic geometry" means.

use crate::clean::{self, CleanStats};
use crate::topo::Topology;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Cross,
    Overlap,
}

/// One located defect, in quantized grid units (see [`Bad::locs`]).
#[derive(Clone, Copy, Debug)]
pub struct Loc {
    /// ring ordinal within the measured iteration
    pub ring: usize,
    pub kind: Kind,
    pub x: f64,
    pub y: f64,
}

#[derive(Default, Clone)]
pub struct Bad {
    /// non-adjacent segment pairs that properly cross
    pub crossings: usize,
    /// non-adjacent collinear segment pairs overlapping in more than a point
    pub overlaps: usize,
    /// non-adjacent segment pairs touching in exactly one point
    pub touches: usize,
    /// rings with < 3 distinct vertices or zero area
    pub degenerate: usize,
    pub verts: usize,
    /// crossing/overlap spots (touches are counted but not located)
    pub locs: Vec<Loc>,
}

pub fn measure(rings: impl Iterator<Item = Vec<(i32, i32)>>) -> Bad {
    let mut b = Bad::default();
    for (ri, c) in rings.enumerate() {
        b.verts += c.len();
        if clean::ring_degenerate(&c) {
            b.degenerate += 1;
            continue;
        }
        ring_bad(ri, &c, &mut b);
    }
    b
}

/// A surviving defect of one feature's geometry, in degrees — what the
/// viewer's problems panel lists.
#[derive(Clone, Copy, Debug)]
pub struct Problem {
    pub lon: f64,
    pub lat: f64,
    pub kind: Kind,
    /// owning feature index
    pub feat: usize,
}

/// Run the encoder's quantize → clean → drop pipeline on already-simplified
/// arcs at `qbits`, then locate every surviving crossing/overlap. A spot on a
/// shared border is reported once per owning ring (dedupe by coordinates for
/// display).
#[must_use]
pub fn find_problems(t: &Topology, arc_coords: &[Vec<(f64, f64)>], qbits: u32) -> Vec<Problem> {
    #[expect(clippy::cast_precision_loss, reason = "qmax = 2^(qbits-1)-1 ≤ 2^31-1, exact in f64")]
    let qmax = ((1u64 << (qbits - 1)) - 1) as f64;
    let mut cst = CleanStats::default();
    #[expect(clippy::cast_possible_truncation, reason = "lon/lat bounded, products < i32::MAX; float as saturates")]
    let quantize = |&(x, y): &(f64, f64)| (((x / 180.0 * qmax).round()) as i32, ((y / 90.0 * qmax).round()) as i32);
    let arcs_q: Vec<Vec<(i32, i32)>> = arc_coords.iter().map(|a| {
        let mut q: Vec<(i32, i32)> = a.iter()
            .map(quantize)
            .collect();
        let closed = a.len() > 1 && a.first() == a.last();
        clean::clean_arc(&mut q, closed, &mut cst);
        q
    }).collect();
    let (ring_refs, structure, arcs_q) =
        clean::drop_degenerate_rings(&t.ring_refs, &t.structure, arcs_q, &mut cst);

    let mut owner = vec![usize::MAX; ring_refs.len()];
    for (fi, f) in structure.iter().enumerate() {
        for poly in f {
            for &ri in poly {
                owner[ri] = fi;
            }
        }
    }
    let bad = measure(ring_refs.iter().map(|r| clean::ring_coords_q(r, &arcs_q)));
    bad.locs.iter().map(|l| Problem {
        lon: l.x / qmax * 180.0,
        lat: l.y / qmax * 90.0,
        kind: l.kind,
        feat: owner[l.ring],
    }).collect()
}

/// Count non-adjacent segment pairs of one ring that intersect, split by
/// kind. Sweep over min-x-sorted segments — O(n log n + pairs-in-x-overlap),
/// fine at report scale. Crossing/overlap spots land in `b.locs`.
///
/// # Panics
/// If a ring has more than `u32::MAX` segments.
pub fn ring_bad(ri: usize, c: &[(i32, i32)], b: &mut Bad) {
    let n = c.len();
    if n < 4 {
        return;
    }
    let seg = |i: usize| (c[i], c[(i + 1) % n]);
    let minx = |i: usize| { let (p, q) = seg(i); p.0.min(q.0) };
    let mut idx: Vec<u32> = (0..u32::try_from(n).expect("segment count fits u32")).collect();
    idx.sort_unstable_by_key(|&i| minx(i as usize));
    for ai in 0..n {
        let i = idx[ai] as usize;
        let (p1, p2) = seg(i);
        let (maxx, ymin, ymax) = (p1.0.max(p2.0), p1.1.min(p2.1), p1.1.max(p2.1));
        for &jj in &idx[ai + 1..] {
            let j = jj as usize;
            let (q1, q2) = seg(j);
            if q1.0.min(q2.0) > maxx {
                break;
            }
            if i.abs_diff(j) == 1 || i.abs_diff(j) == n - 1 {
                continue; // adjacent segments share a vertex by construction
            }
            if q1.1.max(q2.1) < ymin || q1.1.min(q2.1) > ymax {
                continue;
            }
            match seg_class((p1, p2), (q1, q2)) {
                Class::Cross => {
                    b.crossings += 1;
                    let (x, y) = cross_point((p1, p2), (q1, q2));
                    b.locs.push(Loc { ring: ri, kind: Kind::Cross, x, y });
                }
                Class::Overlap => {
                    b.overlaps += 1;
                    // midpoint of the shared stretch: average the two middle
                    // endpoints along the sort order
                    let mut pts = [p1, p2, q1, q2];
                    pts.sort_unstable();
                    let (x, y) = (
                        f64::midpoint(f64::from(pts[1].0), f64::from(pts[2].0)),
                        f64::midpoint(f64::from(pts[1].1), f64::from(pts[2].1)),
                    );
                    b.locs.push(Loc { ring: ri, kind: Kind::Overlap, x, y });
                }
                Class::Touch => b.touches += 1,
                Class::None => {}
            }
        }
    }
}

/// Intersection point of two properly crossing segments (denominator is
/// nonzero exactly because they properly cross).
fn cross_point(
    (p1, p2): ((i32, i32), (i32, i32)),
    (q1, q2): ((i32, i32), (i32, i32)),
) -> (f64, f64) {
    let (dx, dy) = (f64::from(p2.0 - p1.0), f64::from(p2.1 - p1.1));
    let (ex, ey) = (f64::from(q2.0 - q1.0), f64::from(q2.1 - q1.1));
    let denom = dx * ey - dy * ex;
    let t = (f64::from(q1.0 - p1.0) * ey - f64::from(q1.1 - p1.1) * ex) / denom;
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

fn sgn(v: i128) -> i8 {
    i8::from(v > 0) - i8::from(v < 0)
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
        let val = |p: (i32, i32)| if flat { p.1 } else { p.0 };
        let (a0, a1) = (val(p1).min(val(p2)), val(p1).max(val(p2)));
        let (b0, b1) = (val(q1).min(val(q2)), val(q1).max(val(q2)));
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
        let b = measure(vec![ring].into_iter());
        assert_eq!(b.crossings, 1);
        assert_eq!(b.locs.len(), 1);
        let l = b.locs[0];
        assert_eq!((l.x, l.y), (5.0, 5.0));
    }

    #[test]
    fn square_ring_is_clean() {
        let ring = vec![(0, 0), (10, 0), (10, 10), (0, 10)];
        let b = measure(vec![ring].into_iter());
        assert_eq!((b.crossings, b.overlaps, b.touches, b.degenerate), (0, 0, 0, 0));
    }

    #[test]
    fn find_problems_locates_bowtie_in_degrees() {
        // one feature, one ring stored as a single closed arc, bowtie shape
        // big enough to survive i16 snapping (~0.005° cells)
        let arc: Vec<(f64, f64)> = vec![
            (0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0),
        ];
        let t = Topology {
            arc_coords: vec![arc.clone()],
            ring_refs: vec![vec![0 << 1]],
            structure: vec![vec![vec![0]]],
        };
        let p = find_problems(&t, &t.arc_coords, 16);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].feat, 0);
        assert!(matches!(p[0].kind, Kind::Cross));
        assert!((p[0].lon - 0.5).abs() < 0.01 && (p[0].lat - 0.5).abs() < 0.01, "{:?}", p[0]);
    }
}
