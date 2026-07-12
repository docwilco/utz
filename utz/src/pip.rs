//! Hand-rolled per-polygon integer point-in-polygon (PLAN.md §8).
//!
//! Even-odd ray cast (ray toward +x) over ONE polygon's rings — exterior
//! first, holes after; parity XORs across rings, so a point inside a hole
//! comes out `false`. Exact integer arithmetic, no division: one cross
//! product `(b-a)×(p-a)` in a wide type decides crossing direction AND
//! boundary (collinear) for each edge whose y-span touches the scanline.
//!
//! One generic kernel (§14.11), two type axes:
//!
//! `W` is the wide product type (overflow bound: product ≤ `4·coord_max²`):
//! - `i64` — safe for i16/i24 grids (|coord| ≤ 2^23 → products ≤ 2^48).
//!   Note i16-narrow storage does NOT relax this to i32: |coord| ≤ 2^15
//!   still means differences up to 2^16 and a cross up to 2^33 — the narrow
//!   win is memory and load width, not product width.
//! - `i128` — for i32-fine grids (deg×1e7 overflows i64)
//! - `f64` — **test/bench only**, never dispatched by lookup.
//!   Bit-exact for i16/i24 (products ≤ 2^48 < 2^53 — every product and
//!   difference representable), silently inexact near boundaries at i32
//!   (products ~2^62; the same failure mode as geometry-rs's float tests,
//!   §15). Trade-off in one line: integer buys exact sign decisions at every
//!   width and zero FPU dependency (soft-float parts, f32-only FPUs) at the
//!   cost of double-width products; f64 is IEEE-deterministic and
//!   SIMD-friendly on hosts but needs this exactness bound *proven* per
//!   quant width — and on FPU-less cores it is the slow path (measured on
//!   ESP32-S3, §15).
//!
//! The coordinate storage is a [`CoordPair`]: decoded `(i32, i32)` pairs,
//! `(i16, i16)` pairs at quant width (i16-quant eager cache and image
//! sections — §14.11, half the RAM/flash traffic), or packed [`Pack24`]
//! straight over image bytes. The kernel widens each vertex as it loads it;
//! coordinate comparisons run at the narrow width.
//!
//! i16 pairs additionally get a dedicated 32-bit kernel ([`edge_i16`] /
//! [`ring_hit_i16`]): the wide type disappears into an exact u32
//! sign-magnitude compare. Strictly a 32-bit-core kernel — 0.75× the i64
//! kernel on the ESP32-S3 but 2.3× on `x86_64`, where a wide multiply is one
//! instruction and the extra branches only cost (§15) — so the finder
//! dispatches i16-quant lookups to it on `target_pointer_width = "32"`
//! only; verdicts are identical either way.
//!
//! Points exactly ON any edge (exterior or hole) report `true`: border points
//! are ambiguous between adjacent zones, and claiming them keeps lookup
//! deterministic (first candidate polygon wins).
//!
//! Three granularities, one kernel (§9 memory modes):
//! - [`contains`] — whole polygon from ring slices.
//! - [`ring_hit`] — one ring (eager cache / image sections).
//! - [`edge`] — one edge, the streaming unit (§14.7): the test is
//!   per-segment, endpoint-symmetric, and parity accumulation is
//!   order-independent, so lazy/static lookups fold arcs through it straight
//!   off the container bytes with O(1) state and no decode buffer.

use core::ops::{Mul, Sub};

/// Trait alias for the kernel's wide product type — all pure core traits
/// (§14.11), blanket-implemented, so `i64`/`i128`/`f64` qualify wherever the
/// narrow coordinate type `N` converts in losslessly.
pub trait Wide<N>: Copy + PartialOrd + From<N> + Sub<Output = Self> + Mul<Output = Self> {}
impl<N, W: Copy + PartialOrd + From<N> + Sub<Output = W> + Mul<Output = W>> Wide<N> for W {}

/// Coordinate-pair storage the kernels widen from: pairs are stored at quant
/// width (§14.11) — i16/i32 as typed tuples, i24 packed ([`Pack24`]).
pub trait CoordPair: Copy {
    /// The narrow in-memory coordinate type; widened to `W` per edge.
    type Narrow: Copy + Ord;
    fn xy(&self) -> (Self::Narrow, Self::Narrow);
}

impl CoordPair for (i32, i32) {
    type Narrow = i32;
    #[expect(clippy::inline_always, reason = "per-vertex accessor in the streaming PIP hot loop; keep codegen deterministic on Xtensa")]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        *self
    }
}
impl CoordPair for (i16, i16) {
    type Narrow = i16;
    #[expect(clippy::inline_always, reason = "per-vertex accessor in the streaming PIP hot loop; keep codegen deterministic on Xtensa")]
    #[inline(always)]
    fn xy(&self) -> (i16, i16) {
        *self
    }
}

/// Ring-level verdict: `Inside` toggles polygon parity, `Boundary` claims.
pub enum RingHit {
    Inside,
    Outside,
    Boundary,
}

/// Edge-level verdict for streaming accumulation.
pub enum EdgeHit {
    Cross,
    Miss,
    Boundary,
}

/// `rings[0]` = exterior, rest = holes; no duplicated closing vertex.
pub fn contains<W, P>(rings: &[&[P]], px: P::Narrow, py: P::Narrow) -> bool
where
    P: CoordPair,
    W: Wide<P::Narrow>,
{
    let mut inside = false;
    for ring in rings {
        match ring_hit::<W, P>(ring, px, py) {
            RingHit::Boundary => return true,
            RingHit::Inside => inside = !inside,
            RingHit::Outside => {}
        }
    }
    inside
}

/// Even-odd scan of one OPEN ring (the closing edge `last→first` is
/// implied). `Inside` = odd crossings of the +x ray from `(px, py)`.
pub fn ring_hit<W, P>(ring: &[P], px: P::Narrow, py: P::Narrow) -> RingHit
where
    P: CoordPair,
    W: Wide<P::Narrow>,
{
    // TODO: Are we actually allowing 2 vertex rings in the data? This seems like a useless extra check.
    let n = ring.len();
    if n < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[n - 1].xy();
    for p in ring {
        let (x1, y1) = p.xy();
        match edge::<W, _>((x0, y0), (x1, y1), px, py) {
            EdgeHit::Boundary => return RingHit::Boundary,
            EdgeHit::Cross => inside = !inside,
            EdgeHit::Miss => {}
        }
        (x0, y0) = (x1, y1);
    }
    if inside {
        RingHit::Inside
    } else {
        RingHit::Outside
    }
}

/// One edge vs the +x ray through `(px, py)`.
///
/// Compute the cross product for any edge whose y-span touches the
/// scanline; collinear + x-in-span = exactly on the edge (covers
/// interior points, vertices, and horizontal edges — every vertex is
/// the endpoint of some touching edge). Crossing rules (each crossing
/// vertex counted once): an upward edge excludes its final endpoint,
/// a downward edge excludes its starting endpoint, horizontal edges
/// never cross. Direction-symmetric in `a`/`b` by construction.
#[expect(clippy::inline_always, reason = "the per-edge kernel every PIP loop folds through; keep codegen deterministic on Xtensa")]
#[inline(always)]
pub fn edge<W, N>(a: (N, N), b: (N, N), px: N, py: N) -> EdgeHit
where
    N: Copy + Ord,
    W: Wide<N>,
{
    let ((x0, y0), (x1, y1)) = (a, b);
    // sign(cross) by comparing the two products of `(b-a)×(p-a)` instead of
    // subtracting: needs no zero constant in `W`
    let cross = || {
        (
            (W::from(x1) - W::from(x0)) * (W::from(py) - W::from(y0)),
            (W::from(y1) - W::from(y0)) * (W::from(px) - W::from(x0)),
        )
    };
    if y0 <= py {
        if y1 >= py {
            let (lhs, rhs) = cross();
            if lhs == rhs {
                if (x0.min(x1) <= px) && (px <= x0.max(x1)) {
                    return EdgeHit::Boundary;
                }
            } else if lhs > rhs && y1 != py {
                return EdgeHit::Cross; // point strictly left of an upward edge
            }
        }
    } else if y1 <= py {
        let (lhs, rhs) = cross();
        if lhs == rhs {
            if (x0.min(x1) <= px) && (px <= x0.max(x1)) {
                return EdgeHit::Boundary;
            }
        } else if lhs < rhs {
            return EdgeHit::Cross; // point strictly right of a downward edge
        }
    }
    EdgeHit::Miss
}

/// [`ring_hit`] fold over the 32-bit [`edge_i16`] kernel — what i16-quant
/// eager/image lookups dispatch on 32-bit targets (module docs; the finder
/// keeps the per-target policy).
#[must_use]
pub fn ring_hit_i16(ring: &[(i16, i16)], px: i16, py: i16) -> RingHit {
    let n = ring.len();
    if n < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[n - 1];
    for &(x1, y1) in ring {
        match edge_i16((x0, y0), (x1, y1), px, py) {
            EdgeHit::Boundary => return RingHit::Boundary,
            EdgeHit::Cross => inside = !inside,
            EdgeHit::Miss => {}
        }
        (x0, y0) = (x1, y1);
    }
    if inside {
        RingHit::Inside
    } else {
        RingHit::Outside
    }
}

/// 32-bit exact edge kernel for i16 coordinates — [`edge`] without the wide
/// type (§14.11/§15): sign-magnitude comparison of the cross product's two
/// halves fits u32 EXACTLY (|diff| ≤ 65535 and 65535² < 2^32 — the compare
/// form needs 2b bits where the subtract form needs 2b+2). Measured 0.75×
/// the i64 kernel on the ESP32-S3, 2.3× (slower) on `x86_64` — the 32-bit
/// MULLs beat the extra branches only where a wide multiply isn't a single
/// instruction, which is why lookups use it on 32-bit targets alone (§15).
///
/// Normalizes the edge upward first: swapping endpoints negates the cross
/// product, folding [`edge`]'s up/down branches into one, and the upward
/// `y1 != py` endpoint-exclusion guard is vacuously true for swapped
/// (originally downward) edges. Per-edge `Cross` verdicts match [`edge`]
/// exactly; `Boundary` may fire from a different edge of the same shared
/// vertex, which ring-level verdicts can't observe (`Boundary`
/// short-circuits the ring).
#[expect(clippy::inline_always, reason = "the per-edge kernel every PIP loop folds through; keep codegen deterministic on Xtensa")]
#[inline(always)]
#[must_use]
pub fn edge_i16(a: (i16, i16), b: (i16, i16), px: i16, py: i16) -> EdgeHit {
    let ((mut x0, mut y0), (mut x1, mut y1)) = (a, b);
    if y0 > y1 {
        core::mem::swap(&mut x0, &mut x1);
        core::mem::swap(&mut y0, &mut y1);
    }
    if py < y0 || py > y1 {
        return EdgeHit::Miss;
    }
    let dx = i32::from(x1) - i32::from(x0);
    let dy = i32::from(y1) - i32::from(y0); // ≥ 0
    let t = i32::from(py) - i32::from(y0); // 0..=dy
    let v = i32::from(px) - i32::from(x0);
    // cross = dx·t − dy·v; both magnitudes ≤ 65535² < 2^32, exact in u32
    let mag_l = dx.unsigned_abs() * t.unsigned_abs();
    let mag_r = dy.unsigned_abs() * v.unsigned_abs();
    let l_neg = dx < 0 && mag_l != 0;
    let r_neg = v < 0 && mag_r != 0;
    let (gt, eq) = match (l_neg, r_neg) {
        (false, false) => (mag_l > mag_r, mag_l == mag_r),
        (true, true) => (mag_l < mag_r, mag_l == mag_r),
        (true, false) => (false, false), // lhs < 0 ≤ rhs
        (false, true) => (true, false),  // lhs ≥ 0 > rhs
    };
    if eq {
        if x0.min(x1) <= px && px <= x0.max(x1) {
            return EdgeHit::Boundary;
        }
    } else if gt && y1 != py {
        return EdgeHit::Cross; // point strictly left of the upward edge
    }
    EdgeHit::Miss
}

/// Packed little-endian i24 pair (6 bytes, align 1): `&[Pack24]` is a valid
/// slice over the image bytes at ANY address — i24 images have no alignment
/// requirement. The unpack compiles to whatever the target does best
/// (single unaligned-style loads on x86/ARMv7-M+, byte assembly on strict
/// cores — measured a tie with hand-blocked aligned loads on Xtensa, §15).
#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg(feature = "geom-image")]
pub struct Pack24(pub [u8; 6]);

#[cfg(feature = "geom-image")]
impl CoordPair for Pack24 {
    type Narrow = i32;
    #[expect(clippy::inline_always, reason = "per-vertex accessor in the streaming PIP hot loop; keep codegen deterministic on Xtensa")]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        // two overlapping in-struct word reads (x = low 3 bytes of the first,
        // y = high 3 of the second); read_unaligned is a single load where
        // hardware allows, byte assembly on strict-alignment cores (measured
        // a tie with hand-blocked aligned loads on Xtensa — §15). Arithmetic
        // shifts sign-extend.
        let p = self.0.as_ptr();
        // SAFETY: both 4-byte reads lie within this 6-byte struct
        let (xw, yw) = unsafe {
            (
                u32::from_le(p.cast::<u32>().read_unaligned()),
                u32::from_le(p.add(2).cast::<u32>().read_unaligned()),
            )
        };
        ((xw << 8).cast_signed() >> 8, yw.cast_signed() >> 8)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    const SQUARE: &[(i32, i32)] = &[(0, 0), (10, 0), (10, 10), (0, 10)];
    const HOLE: &[(i32, i32)] = &[(3, 3), (7, 3), (7, 7), (3, 7)];

    #[test]
    fn square_basics() {
        assert!(contains::<i64, _>(&[SQUARE], 5, 5));
        assert!(!contains::<i64, _>(&[SQUARE], 15, 5));
        assert!(!contains::<i64, _>(&[SQUARE], -1, -1));
        // boundary claims: edges + vertices
        assert!(contains::<i64, _>(&[SQUARE], 0, 5));
        assert!(contains::<i64, _>(&[SQUARE], 5, 0));
        assert!(contains::<i64, _>(&[SQUARE], 10, 10));
    }

    #[test]
    fn hole_excludes() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        assert!(contains::<i64, _>(poly, 1, 1)); // in exterior, outside hole
        assert!(!contains::<i64, _>(poly, 5, 5)); // inside hole
        assert!(contains::<i64, _>(poly, 3, 5)); // on hole edge -> claimed
    }

    #[test]
    fn i128_matches_i64_in_range() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        for x in -2..13 {
            for y in -2..13 {
                assert_eq!(contains::<i64, _>(poly, x, y), contains::<i128, _>(poly, x, y), "at ({x},{y})");
            }
        }
    }

    #[test]
    fn f64_matches_i64_in_range() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        for x in -2..13 {
            for y in -2..13 {
                assert_eq!(contains::<i64, _>(poly, x, y), contains::<f64, _>(poly, x, y), "at ({x},{y})");
            }
        }
    }

    /// i16-narrow pairs through the i64 kernel AND the 32-bit sign-split
    /// kernel must agree with the identical geometry widened to i32 pairs —
    /// full i16 range, so the worst-case cross (2^33 in the subtract form,
    /// 65535² in [`edge_i16`]'s compare form) is exercised.
    #[test]
    fn narrow_i16_matches_i32_pairs() {
        let mut lcg = utz_common::Lcg::new(0x1616_1616);
        // top 16 LCG bits, reinterpreted over the full i16 range
        let mut next = || ((lcg.next_u64() >> 48) as u16).cast_signed();
        for _ in 0..200 {
            let n = 3 + (next().unsigned_abs() as usize % 14);
            let ring16: Vec<(i16, i16)> = (0..n).map(|_| (next(), next())).collect();
            let ring32: Vec<(i32, i32)> =
                ring16.iter().map(|&(x, y)| (i32::from(x), i32::from(y))).collect();
            let code = |h: RingHit| match h {
                RingHit::Outside => 0,
                RingHit::Inside => 1,
                RingHit::Boundary => 2,
            };
            for _ in 0..200 {
                let (px, py) = (next(), next());
                let wide = code(ring_hit::<i64, _>(&ring32, i32::from(px), i32::from(py)));
                assert_eq!(
                    code(ring_hit::<i64, _>(&ring16, px, py)),
                    wide,
                    "i16/i32 verdicts disagree at ({px},{py})"
                );
                assert_eq!(
                    code(ring_hit_i16(&ring16, px, py)),
                    wide,
                    "sign-split verdict disagrees at ({px},{py})"
                );
            }
        }
    }

    /// f64 is bit-exact at i24 range (products ≤ 2^48 < 2^53 — module docs),
    /// so agreement with i64 over full-range random polygons is a hard
    /// assertion, boundaries included.
    #[test]
    #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap, reason = "test PRNG: values constructed within i24/i32 range")]
    fn f64_matches_i64_at_i24_range() {
        const M: i64 = 1 << 23; // i24 coordinate range
        let mut lcg = utz_common::Lcg::new(0x0dd_ba11);
        let mut next = |m: i64| -> i32 { (((lcg.next_u64() >> 33) as i64 % m) - m / 2) as i32 };
        for _ in 0..200 {
            let (cx, cy) = (next(M), next(M));
            let n = 5 + (next(12).unsigned_abs() as usize);
            let mut pts: Vec<(i32, i32)> = (0..n)
                .map(#[expect(clippy::cast_precision_loss, reason = "test geometry: k < n ≤ 17 and radius r < 2^12+2^20, all exact in f64")] |k| {
                    let ang = k as f64 / n as f64 * core::f64::consts::TAU;
                    let r = (1 << 12) + i64::from(next(1 << 20).unsigned_abs());
                    (
                        (i64::from(cx) + (ang.cos() * r as f64) as i64).clamp(-M, M - 1) as i32,
                        (i64::from(cy) + (ang.sin() * r as f64) as i64).clamp(-M, M - 1) as i32,
                    )
                })
                .collect();
            pts.dedup();
            if pts.first() == pts.last() {
                pts.pop();
            }
            if pts.len() < 3 {
                continue;
            }
            let rings: &[&[(i32, i32)]] = &[&pts];
            for _ in 0..200 {
                let (px, py) = (cx.saturating_add(next(1 << 21)), cy.saturating_add(next(1 << 21)));
                assert_eq!(
                    contains::<i64, _>(rings, px, py),
                    contains::<f64, _>(rings, px, py),
                    "f64/i64 disagree at ({px},{py})"
                );
            }
        }
    }

    /// cross-validate against the geo i64 oracle (PLAN.md §8) on random
    /// integer polygons — interiors must agree everywhere off-boundary.
    #[test]
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap, reason = "test PRNG: values constructed within i24/i32 range")]
    fn geo_oracle_agreement() {
        use geo::Contains;
        let mut lcg = utz_common::Lcg::new(0xdead_beef);
        let mut next = |m: i64| -> i32 { ((lcg.next_u64() >> 33) as i64 % m) as i32 };
        for _ in 0..200 {
            // random star-shaped polygon (guaranteed simple) around a center
            let (cx, cy) = (next(1000) - 500, next(1000) - 500);
            let n = 5 + (next(12) as usize);
            let mut pts: Vec<(i32, i32)> = (0..n)
                .map(#[expect(clippy::cast_precision_loss, reason = "test geometry: k < n ≤ 17 and radius r ≤ 450, exact in f64")] |k| {
                    let ang = k as f64 / n as f64 * core::f64::consts::TAU;
                    let r = 50 + i64::from(next(400));
                    (cx + (ang.cos() * r as f64) as i32, cy + (ang.sin() * r as f64) as i32)
                })
                .collect();
            pts.dedup();
            if pts.first() == pts.last() { pts.pop(); }
            if pts.len() < 3 { continue; }

            let ext: geo::LineString<i64> = pts.iter().map(|&(x, y)| (i64::from(x), i64::from(y))).collect();
            let gpoly = geo::Polygon::new(ext, vec![]);
            let rings: &[&[(i32, i32)]] = &[&pts];
            for _ in 0..200 {
                let (px, py) = (cx + next(1200) - 600, cy + next(1200) - 600);
                let ours = contains::<i64, _>(rings, px, py);
                let geo_says = gpoly.contains(&geo::Point::new(i64::from(px), i64::from(py)));
                if ours != geo_says {
                    // geo::Contains excludes the boundary; we claim it. Only
                    // that exact disagreement is allowed.
                    use geo::algorithm::Intersects;
                    let on_boundary = gpoly.exterior().intersects(&geo::Point::new(i64::from(px), i64::from(py)));
                    assert!(ours && on_boundary, "disagree off-boundary at ({px},{py})");
                }
            }
        }
    }
}
