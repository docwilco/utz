//! Integer point-in-polygon kernels, the geometric core behind
//! [`lookup()`](crate::Finder::lookup). Nothing in this module is needed
//! to use μTZ. It is public so that the benches can drive the kernels
//! directly and so that the exactness claims documented on the items
//! below can be audited.
//!
//! # Algorithm
//!
//! Every kernel performs the classic even-odd ray cast: a ray from the
//! query point toward +x is tested against every edge of one polygon's
//! rings, exterior first and holes after, and an odd crossing count means
//! the point is inside. Parity XORs across rings, so a point inside a
//! hole comes out `false`. The arithmetic is exact, integer-only, and
//! free of division: for each edge whose y-span touches the scanline, a
//! single cross product `(b-a)×(p-a)` computed in a wide type decides
//! both the crossing direction and whether the point lies exactly on the
//! edge (the collinear case).
//!
//! A point exactly on any edge, exterior or hole, reports `true`. Border
//! points are ambiguous between adjacent zones, and claiming them keeps
//! lookup deterministic: the first candidate polygon wins.
//!
//! # Two kernel families
//!
//! The module implements the ray cast twice, shaped for different cores.
//! The wide-product family computes each cross product in a double-width
//! type and is the fast path wherever a wide multiply is cheap. The
//! sign-split family avoids the wide type altogether and is the fast path
//! on 32-bit cores. The finder dispatches lookups to the sign-split
//! family when `target_pointer_width = "32"` and to the wide-product
//! family everywhere else; the results are identical either way.
//!
//! The wide-product kernels ([`contains`], [`ring_hit`], and [`edge`])
//! share one generic implementation with two type parameters. `W` is the
//! wide type the cross product is formed in: `i64`, `i128`, or, for tests
//! and benches only, `f64`; the [`Wide`] trait documents which width is
//! exact for which coordinate grid. `P` is the coordinate-pair storage
//! that vertices are loaded from: decoded `(i32, i32)`, narrow
//! `(i16, i16)`, or packed `Pack24`; the [`CoordPair`] trait documents
//! the layouts. The kernel widens each vertex as it loads it, and
//! coordinate comparisons run at the narrow width.
//!
//! The sign-split kernels ([`ring_hit_split`] and [`edge_split`]) mirror
//! [`ring_hit`] and [`edge`] with `W` removed: each cross-product half is
//! split into a sign, decided by narrow comparisons, and an exact
//! unsigned magnitude product that fits in twice the coordinate width.
//! The [`Narrow`] trait supplies that product type for each coordinate
//! type, and [`edge_split`] documents why this form wins on 32-bit cores
//! and loses on 64-bit hosts.
//!
//! # Three granularities
//!
//! Each memory mode enters through its own granularity, and all three are
//! folds of the same edge test:
//!
//! - [`contains`] tests a whole polygon given its ring slices.
//! - [`ring_hit`] tests one ring, and serves the eager cache and the
//!   full-rings sections.
//! - [`edge`] tests one edge and is the streaming unit. The test is
//!   per-segment and endpoint-symmetric, and parity accumulation is
//!   order-independent, so lazy and static lookups can fold arcs through
//!   it straight off the asset bytes with O(1) state and no decode
//!   buffer.

use core::ops::{Mul, Sub};

/// The wide type the kernels form cross products in. Every cross-product
/// half is a difference multiplied by a difference, so `W` must represent
/// `4·coord_max²` exactly. The trait is an alias built from pure core
/// traits and blanket-implemented, so any type qualifies wherever the
/// narrow coordinate type `N` converts in losslessly.
///
/// Which width is exact for which grid:
///
/// - `i64` is exact for the i16 and i24 grids: with |coord| ≤ 2^23, every
///   product stays at or below 2^48. Note that i16-narrow storage does
///   not relax the requirement to `i32`. With |coord| ≤ 2^15 the
///   differences still reach 2^16 and a cross product reaches 2^33; what
///   narrow storage buys is memory and load width, not a smaller product.
/// - `i128` serves the i32-fine grid, where degrees ×1e7 overflow `i64`.
/// - `f64` is for tests and benches only and is never dispatched by
///   lookup. It is bit-exact for i16/i24, where every product and
///   difference stays below 2^53 and is therefore representable, but it
///   is silently inexact near boundaries at i32, where products approach
///   2^62. That is the same failure mode as geometry-rs's float tests.
///
/// The integer default trades double-width products for exact sign
/// decisions at every width and zero FPU dependency, which matters on
/// soft-float parts and f32-only FPUs. `f64` is IEEE-deterministic and
/// SIMD-friendly on hosts, but its exactness bound has to be proven for
/// each quant width, and on FPU-less cores it is the slow path.
pub trait Wide<N>: Copy + PartialOrd + From<N> + Sub<Output = Self> + Mul<Output = Self> {}
impl<N, W: Copy + PartialOrd + From<N> + Sub<Output = W> + Mul<Output = W>> Wide<N> for W {}

/// The coordinate-pair storage the kernels load vertices from. Pairs are
/// always stored at quant width: as decoded `(i32, i32)` tuples, as
/// `(i16, i16)` tuples for the i16-quant eager cache and full-rings
/// sections (half the RAM and flash traffic), or as packed `Pack24` read
/// directly over full-rings section bytes.
pub trait CoordPair: Copy {
    /// The narrow in-memory coordinate type, which the kernel widens to
    /// `W` as it loads each vertex.
    type Narrow: Copy + Ord;
    fn xy(&self) -> (Self::Narrow, Self::Narrow);
}

impl CoordPair for (i32, i32) {
    type Narrow = i32;
    #[expect(
        clippy::inline_always,
        reason = "per-vertex accessor in the streaming PIP hot loop; inlining must not depend on per-target heuristics"
    )]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        *self
    }
}
impl CoordPair for (i16, i16) {
    type Narrow = i16;
    #[expect(
        clippy::inline_always,
        reason = "per-vertex accessor in the streaming PIP hot loop; inlining must not depend on per-target heuristics"
    )]
    #[inline(always)]
    fn xy(&self) -> (i16, i16) {
        *self
    }
}

/// The result for one ring. `Inside` toggles the polygon's parity, and
/// `Boundary` claims the point immediately.
pub enum RingHit {
    /// The +x ray crossed this ring an odd number of times, so the
    /// polygon's parity toggles.
    Inside,
    /// The point is outside this ring.
    Outside,
    /// The point lies exactly on the ring and is claimed immediately.
    Boundary,
}

/// The result for one edge, the unit of streaming accumulation.
pub enum EdgeHit {
    /// The +x ray crosses this edge, so parity toggles.
    Cross,
    /// The ray does not cross this edge.
    Miss,
    /// The point lies exactly on this edge and is claimed immediately.
    Boundary,
}

/// Reports whether the point `(px, py)` is inside the polygon whose rings
/// are given as slices. `rings[0]` is the exterior and the remaining
/// rings are holes. Rings are open: the closing vertex is not duplicated.
#[must_use]
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

/// Scans one open ring with the even-odd rule; the closing edge from the
/// last vertex back to the first is implied. The result is `Inside` when
/// the +x ray from `(px, py)` crosses an odd number of edges.
#[must_use]
pub fn ring_hit<W, P>(ring: &[P], px: P::Narrow, py: P::Narrow) -> RingHit
where
    P: CoordPair,
    W: Wide<P::Narrow>,
{
    // TODO: Are we actually allowing 2 vertex rings in the data? This seems like a useless extra check.
    let vertex_count = ring.len();
    if vertex_count < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[vertex_count - 1].xy();
    for vertex in ring {
        let (x1, y1) = vertex.xy();
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

/// Tests one edge against the +x ray through `(px, py)`.
///
/// Only an edge whose y-span touches the scanline is examined. If the
/// point is collinear with the edge and `px` falls within the edge's
/// x-span, the result is `Boundary`; this covers edge interiors,
/// vertices, and horizontal edges, because every vertex is an endpoint of
/// some edge that touches the scanline. Crossings are counted so that a
/// ray through a vertex counts exactly once: an upward edge excludes its
/// final endpoint, a downward edge excludes its starting endpoint, and a
/// horizontal edge never crosses. The test is symmetric in `a` and `b` by
/// construction.
#[expect(
    clippy::inline_always,
    reason = "the per-edge kernel every PIP loop folds through; inlining must not depend on per-target heuristics"
)]
#[inline(always)]
#[must_use]
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

/// Sign-split arithmetic for a narrow coordinate type. [`Self::Product`]
/// is the unsigned type in which one cross-product half fits exactly,
/// because `(2^b−1)² ≤ 2^2b − 1`: `u32` for i16 coordinates and `u64` for
/// i24 and i32. The compare form therefore needs only 2b bits where
/// [`edge`]'s subtract form needs 2b+2, and because the magnitudes are
/// true b-bit `abs_diff`s, each product is a single widening multiply
/// even on a core whose word is b bits wide. The `W` kernels cannot match
/// that there, since their (b+1)-bit differences force full wide
/// multiplies.
pub trait Narrow: Copy + Ord {
    /// The unsigned 2b-bit product type.
    type Product: Copy + Ord;
    /// Computes `|a−b| · |c−d|` exactly.
    fn magnitude_product(a: Self, b: Self, c: Self, d: Self) -> Self::Product;
}
impl Narrow for i16 {
    type Product = u32;
    #[expect(
        clippy::inline_always,
        reason = "per-edge product in the PIP hot loop; inlining must not depend on per-target heuristics"
    )]
    #[inline(always)]
    fn magnitude_product(a: Self, b: Self, c: Self, d: Self) -> u32 {
        u32::from(a.abs_diff(b)) * u32::from(c.abs_diff(d))
    }
}
impl Narrow for i32 {
    type Product = u64;
    #[expect(
        clippy::inline_always,
        reason = "per-edge product in the PIP hot loop; inlining must not depend on per-target heuristics"
    )]
    #[inline(always)]
    fn magnitude_product(a: Self, b: Self, c: Self, d: Self) -> u64 {
        u64::from(a.abs_diff(b)) * u64::from(c.abs_diff(d))
    }
}

/// Runs [`ring_hit`]'s scan with [`edge_split`] in place of [`edge`].
/// This is the ring-level kernel that lookups dispatch to on 32-bit
/// targets; the finder owns the per-target policy.
#[must_use]
pub fn ring_hit_split<P>(ring: &[P], px: P::Narrow, py: P::Narrow) -> RingHit
where
    P: CoordPair,
    P::Narrow: Narrow,
{
    let vertex_count = ring.len();
    if vertex_count < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[vertex_count - 1].xy();
    for vertex in ring {
        let (x1, y1) = vertex.xy();
        match edge_split((x0, y0), (x1, y1), px, py) {
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

/// The sign-split edge kernel, which is [`edge`] with the wide type
/// removed: each cross-product half is split into a sign, decided by
/// narrow comparisons, and an exact unsigned magnitude product
/// ([`Narrow::magnitude_product()`]). It is strictly a 32-bit-core
/// kernel. The i16 instantiation measured 0.75× the i64 kernel's time on
/// a 32-bit core but 2.3× on a 64-bit host, where a wide multiply is a
/// single instruction and the extra branches only cost; that is why
/// lookups use it on 32-bit targets alone.
///
/// Per-edge `Cross` results match [`edge`] exactly. A `Boundary` result
/// may fire from a different edge of the same shared vertex, but
/// ring-level results cannot observe the difference, because `Boundary`
/// short-circuits the ring.
#[expect(
    clippy::inline_always,
    reason = "the per-edge kernel every PIP loop folds through; inlining must not depend on per-target heuristics"
)]
#[inline(always)]
#[must_use]
pub fn edge_split<N: Narrow>(a: (N, N), b: (N, N), px: N, py: N) -> EdgeHit {
    let ((mut x0, mut y0), (mut x1, mut y1)) = (a, b);
    // Normalize the edge upward: swapping endpoints negates the cross
    // product, folding `edge`'s up/down branches into one, and the upward
    // `y1 != py` endpoint-exclusion guard is vacuously true for swapped
    // (originally downward) edges.
    if y0 > y1 {
        core::mem::swap(&mut x0, &mut x1);
        core::mem::swap(&mut y0, &mut y1);
    }
    if py < y0 || py > y1 {
        return EdgeHit::Miss;
    }
    // cross = dx·t − dy·v with dx = x1−x0, dy = y1−y0 ≥ 0, t = py−y0 in
    // 0..=dy, v = px−x0. A product is strictly negative iff its signed
    // factor is negative AND the other is nonzero — all narrow comparisons.
    let lhs_magnitude = N::magnitude_product(x1, x0, py, y0);
    let rhs_magnitude = N::magnitude_product(y1, y0, px, x0);
    let lhs_negative = x1 < x0 && py != y0;
    let rhs_negative = px < x0 && y1 != y0;
    let (greater, equal) = match (lhs_negative, rhs_negative) {
        (false, false) => (
            lhs_magnitude > rhs_magnitude,
            lhs_magnitude == rhs_magnitude,
        ),
        (true, true) => (
            lhs_magnitude < rhs_magnitude,
            lhs_magnitude == rhs_magnitude,
        ),
        (true, false) => (false, false), // lhs < 0 ≤ rhs
        (false, true) => (true, false),  // lhs ≥ 0 > rhs
    };
    if equal {
        if x0.min(x1) <= px && px <= x0.max(x1) {
            return EdgeHit::Boundary;
        }
    } else if greater && y1 != py {
        return EdgeHit::Cross; // point strictly left of the upward edge
    }
    EdgeHit::Miss
}

/// A packed little-endian i24 coordinate pair: 6 bytes with alignment 1.
/// Because the alignment is 1, `&[Pack24]` is a valid slice over the
/// section bytes at any address, so i24 sections carry no alignment
/// requirement. The unpack compiles to whatever the target does best:
/// single unaligned-style loads where the hardware allows them, and byte
/// assembly on strict-alignment cores, where it measured a tie with
/// hand-blocked aligned loads.
#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg(feature = "geom-full-rings")]
pub struct Pack24(pub [u8; 6]);

#[cfg(feature = "geom-full-rings")]
impl CoordPair for Pack24 {
    type Narrow = i32;
    #[expect(
        clippy::inline_always,
        reason = "per-vertex accessor in the streaming PIP hot loop; inlining must not depend on per-target heuristics"
    )]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        // two overlapping in-struct word reads (x = low 3 bytes of the first,
        // y = high 3 of the second); read_unaligned is a single load where
        // hardware allows, byte assembly on strict-alignment cores (measured
        // a tie with hand-blocked aligned loads there). Arithmetic
        // shifts sign-extend.
        let ptr = self.0.as_ptr();
        // SAFETY: both 4-byte reads lie within this 6-byte struct
        let (x_word, y_word) = unsafe {
            (
                u32::from_le(ptr.cast::<u32>().read_unaligned()),
                u32::from_le(ptr.add(2).cast::<u32>().read_unaligned()),
            )
        };
        ((x_word << 8).cast_signed() >> 8, y_word.cast_signed() >> 8)
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
                assert_eq!(
                    contains::<i64, _>(poly, x, y),
                    contains::<i128, _>(poly, x, y),
                    "at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn f64_matches_i64_in_range() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        for x in -2..13 {
            for y in -2..13 {
                assert_eq!(
                    contains::<i64, _>(poly, x, y),
                    contains::<f64, _>(poly, x, y),
                    "at ({x},{y})"
                );
            }
        }
    }

    /// i16-narrow pairs through the i64 kernel AND the 32-bit sign-split
    /// kernel must agree with the identical geometry widened to i32 pairs:
    /// full i16 range, so the worst-case cross (2^33 in the subtract form,
    /// 65535² in [`edge_split`]'s compare form) is exercised.
    #[test]
    fn narrow_i16_matches_i32_pairs() {
        let mut lcg = utz_common::Lcg::new(0x1616_1616);
        // top 16 LCG bits, reinterpreted over the full i16 range
        let mut next = || ((lcg.next_u64() >> 48) as u16).cast_signed();
        for _ in 0..200 {
            let n = 3 + (next().unsigned_abs() as usize % 14);
            let ring16: Vec<(i16, i16)> = (0..n).map(|_| (next(), next())).collect();
            let ring32: Vec<(i32, i32)> = ring16
                .iter()
                .map(|&(x, y)| (i32::from(x), i32::from(y)))
                .collect();
            let code = |hit: RingHit| match hit {
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
                    "i16/i32 results disagree at ({px},{py})"
                );
                assert_eq!(
                    code(ring_hit_split(&ring16, px, py)),
                    wide,
                    "sign-split result disagrees at ({px},{py})"
                );
                assert_eq!(
                    code(ring_hit_split(&ring32, i32::from(px), i32::from(py))),
                    wide,
                    "i32 sign-split result disagrees at ({px},{py})"
                );
            }
        }
    }

    /// At 15-bit quantization (|coord| ≤ 2^14) the plain kernel is exact at
    /// `W = i32`: differences fit 15 bits, each cross-product half fits
    /// 2^30. Agreement with the i64 kernel over that range is the proof,
    /// and this test runs in debug, where an overflow would panic rather
    /// than wrap, so it also guards the bound itself.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test PRNG: values constructed within the 15-bit quant range"
    )]
    fn wide_i32_matches_i64_at_15_bit_quant() {
        const M: i64 = 1 << 15; // draws in [−2^14, 2^14−1]
        let mut lcg = utz_common::Lcg::new(0x1515_1515);
        let mut next = || (((lcg.next_u64() >> 33).cast_signed() % M) - M / 2) as i16;
        let code = |hit: RingHit| match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        };
        for _ in 0..200 {
            let n = 3 + (next().unsigned_abs() as usize % 14);
            let ring: Vec<(i16, i16)> = (0..n).map(|_| (next(), next())).collect();
            for _ in 0..200 {
                let (px, py) = (next(), next());
                assert_eq!(
                    code(ring_hit::<i32, _>(&ring, px, py)),
                    code(ring_hit::<i64, _>(&ring, px, py)),
                    "i32/i64 results disagree at ({px},{py})"
                );
            }
        }
    }

    /// The i32 sign-split instantiation vs the i128 kernel over the FULL
    /// i32 range: products up to `(2^32−1)²`, the u64 exactness bound at
    /// its edge.
    #[test]
    fn split_i32_matches_i128() {
        let mut lcg = utz_common::Lcg::new(0x3232_3232);
        let mut next = || ((lcg.next_u64() >> 32) as u32).cast_signed();
        let code = |hit: RingHit| match hit {
            RingHit::Outside => 0,
            RingHit::Inside => 1,
            RingHit::Boundary => 2,
        };
        for _ in 0..200 {
            let n = 3 + (next().unsigned_abs() as usize % 14);
            let ring: Vec<(i32, i32)> = (0..n).map(|_| (next(), next())).collect();
            for _ in 0..200 {
                let (px, py) = (next(), next());
                assert_eq!(
                    code(ring_hit_split(&ring, px, py)),
                    code(ring_hit::<i128, _>(&ring, px, py)),
                    "i32 sign-split/i128 results disagree at ({px},{py})"
                );
            }
        }
    }

    /// f64 is bit-exact at i24 range (products ≤ 2^48 < 2^53; module docs),
    /// so agreement with i64 over full-range random polygons is a hard
    /// assertion, boundaries included.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "test PRNG: values constructed within i24/i32 range"
    )]
    fn f64_matches_i64_at_i24_range() {
        const M: i64 = 1 << 23; // i24 coordinate range
        let mut lcg = utz_common::Lcg::new(0x0dd_ba11);
        let mut next = |m: i64| -> i32 { (((lcg.next_u64() >> 33) as i64 % m) - m / 2) as i32 };
        for _ in 0..200 {
            let (cx, cy) = (next(M), next(M));
            let n = 5 + (next(12).unsigned_abs() as usize);
            let mut points: Vec<(i32, i32)> = (0..n)
                .map(#[expect(clippy::cast_precision_loss, reason = "test geometry: k < n ≤ 17 and radius r < 2^12+2^20, all exact in f64")] |k| {
                    let angle = k as f64 / n as f64 * core::f64::consts::TAU;
                    let r = (1 << 12) + i64::from(next(1 << 20).unsigned_abs());
                    (
                        (i64::from(cx) + (angle.cos() * r as f64) as i64).clamp(-M, M - 1) as i32,
                        (i64::from(cy) + (angle.sin() * r as f64) as i64).clamp(-M, M - 1) as i32,
                    )
                })
                .collect();
            points.dedup();
            if points.first() == points.last() {
                points.pop();
            }
            if points.len() < 3 {
                continue;
            }
            let rings: &[&[(i32, i32)]] = &[&points];
            for _ in 0..200 {
                let (px, py) = (
                    cx.saturating_add(next(1 << 21)),
                    cy.saturating_add(next(1 << 21)),
                );
                assert_eq!(
                    contains::<i64, _>(rings, px, py),
                    contains::<f64, _>(rings, px, py),
                    "f64/i64 disagree at ({px},{py})"
                );
            }
        }
    }

    /// cross-validate against the geo i64 oracle on random
    /// integer polygons: interiors must agree everywhere off-boundary.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "test PRNG: values constructed within i24/i32 range"
    )]
    fn geo_oracle_agreement() {
        use geo::Contains;
        let mut lcg = utz_common::Lcg::new(0xdead_beef);
        let mut next = |m: i64| -> i32 { ((lcg.next_u64() >> 33) as i64 % m) as i32 };
        for _ in 0..200 {
            // random star-shaped polygon (guaranteed simple) around a center
            let (cx, cy) = (next(1000) - 500, next(1000) - 500);
            let n = 5 + (next(12) as usize);
            let mut points: Vec<(i32, i32)> = (0..n)
                .map(
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "test geometry: k < n ≤ 17 and radius r ≤ 450, exact in f64"
                    )]
                    |k| {
                        let angle = k as f64 / n as f64 * core::f64::consts::TAU;
                        let r = 50 + i64::from(next(400));
                        (
                            cx + (angle.cos() * r as f64) as i32,
                            cy + (angle.sin() * r as f64) as i32,
                        )
                    },
                )
                .collect();
            points.dedup();
            if points.first() == points.last() {
                points.pop();
            }
            if points.len() < 3 {
                continue;
            }

            let exterior: geo::LineString<i64> = points
                .iter()
                .map(|&(x, y)| (i64::from(x), i64::from(y)))
                .collect();
            let geo_polygon = geo::Polygon::new(exterior, vec![]);
            let rings: &[&[(i32, i32)]] = &[&points];
            for _ in 0..200 {
                let (px, py) = (cx + next(1200) - 600, cy + next(1200) - 600);
                let ours = contains::<i64, _>(rings, px, py);
                let geo_says = geo_polygon.contains(&geo::Point::new(i64::from(px), i64::from(py)));
                if ours != geo_says {
                    // geo::Contains excludes the boundary; we claim it. Only
                    // that exact disagreement is allowed.
                    use geo::algorithm::Intersects;
                    let on_boundary = geo_polygon
                        .exterior()
                        .intersects(&geo::Point::new(i64::from(px), i64::from(py)));
                    assert!(ours && on_boundary, "disagree off-boundary at ({px},{py})");
                }
            }
        }
    }
}
