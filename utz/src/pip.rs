//! Hand-rolled per-polygon integer point-in-polygon (PLAN.md §8).
//!
//! Even-odd ray cast (ray toward +x) over ONE polygon's rings — exterior
//! first, holes after; parity XORs across rings, so a point inside a hole
//! comes out `false`. Exact integer arithmetic, no division: one cross
//! product `(b-a)×(p-a)` in a wide type decides crossing direction AND
//! boundary (collinear) for each edge whose y-span touches the scanline.
//!
//! Width follows the quantization grid (overflow bound: product ≤ `4·coord_max²)`:
//! - `contains_i64`  — safe for i16/i24 grids (|coord| ≤ 2^23 → products ≤ 2^48)
//! - `contains_i128` — for i32-fine grids (deg×1e7 overflows i64)
//! - `contains_f64`  — **test/bench only**, never dispatched by lookup.
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
//! Points exactly ON any edge (exterior or hole) report `true`: border points
//! are ambiguous between adjacent zones, and claiming them keeps lookup
//! deterministic (first candidate polygon wins).
//!
//! Three granularities, one kernel (§9 memory modes):
//! - [`contains_i64`]/[`contains_i128`] — whole polygon from ring slices.
//! - [`ring_hit_i64`]/[`ring_hit_i128`] — one decoded ring (eager mode).
//! - [`edge_i64`]/[`edge_i128`] — one edge, the streaming unit (§14.7): the
//!   test is per-segment, endpoint-symmetric, and parity accumulation is
//!   order-independent, so lazy/static lookups fold arcs through it straight
//!   off the container bytes with O(1) state and no decode buffer.

macro_rules! pip_impl {
    ($wide:ident $(, #[$edge_attr:meta])?) => { pastey::paste! {
        /// `rings[0]` = exterior, rest = holes; no duplicated closing vertex.
        pub fn [<contains_ $wide>](rings: &[&[(i32, i32)]], px: i32, py: i32) -> bool {
            let mut inside = false;
            for ring in rings {
                match [<ring_hit_ $wide>](ring, px, py) {
                    RingHit::Boundary => return true,
                    RingHit::Inside => inside = !inside,
                    RingHit::Outside => {}
                }
            }
            inside
        }

        /// Even-odd scan of one OPEN ring (the closing edge `last→first` is
        /// implied). `Inside` = odd crossings of the +x ray from `(px, py)`.
        pub fn [<ring_hit_ $wide>](ring: &[(i32, i32)], px: i32, py: i32) -> RingHit {
            let n = ring.len();
            if n < 3 {
                return RingHit::Outside;
            }
            let mut inside = false;
            let (mut x0, mut y0) = ring[n - 1];
            for i in 0..n {
                let (x1, y1) = ring[i];
                match [<edge_ $wide>]((x0, y0), (x1, y1), px, py) {
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
        #[inline(always)]
        $(#[$edge_attr])?
        pub fn [<edge_ $wide>](a: (i32, i32), b: (i32, i32), px: i32, py: i32) -> EdgeHit {
            let ((x0, y0), (x1, y1)) = (a, b);
            if y0 <= py {
                if y1 >= py {
                    let cross = ($wide::from(x1) - $wide::from(x0)) * ($wide::from(py) - $wide::from(y0))
                        - ($wide::from(y1) - $wide::from(y0)) * ($wide::from(px) - $wide::from(x0));
                    if cross == $wide::from(0) {
                        if (x0.min(x1) <= px) && (px <= x0.max(x1)) {
                            return EdgeHit::Boundary;
                        }
                    } else if cross > $wide::from(0) && y1 != py {
                        return EdgeHit::Cross; // point strictly left of an upward edge
                    }
                }
            } else if y1 <= py {
                let cross = ($wide::from(x1) - $wide::from(x0)) * ($wide::from(py) - $wide::from(y0))
                    - ($wide::from(y1) - $wide::from(y0)) * ($wide::from(px) - $wide::from(x0));
                if cross == $wide::from(0) {
                    if (x0.min(x1) <= px) && (px <= x0.max(x1)) {
                        return EdgeHit::Boundary;
                    }
                } else if cross < $wide::from(0) {
                    return EdgeHit::Cross; // point strictly right of a downward edge
                }
            }
            EdgeHit::Miss
        }
    } };
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

pip_impl!(i64);
pip_impl!(i128);
// test/bench-only instantiation (see module docs); same &[(i32,i32)] slices,
// per-edge i32->f64 casts included in its cost, as any real f64 path would pay
pip_impl!(f64, #[expect(clippy::float_cmp, reason = "cross products are exact in f64 at i24 quant (bit-exact vs i64, tested); zero means on-edge")]);

/// Coordinate-pair storage the `EagerImage` kernels widen from (v7: image
/// coords are stored at quant width — i16/i32 as typed slices, i24 packed).
#[cfg(feature = "geom-image")]
pub trait CoordPair: Copy {
    fn xy(&self) -> (i32, i32);
}
#[cfg(feature = "geom-image")]
impl CoordPair for (i32, i32) {
    #[expect(clippy::inline_always, reason = "per-vertex accessor in the streaming PIP hot loop; keep codegen deterministic on Xtensa")]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        *self
    }
}
#[cfg(feature = "geom-image")]
impl CoordPair for (i16, i16) {
    #[expect(clippy::inline_always, reason = "per-vertex accessor in the streaming PIP hot loop; keep codegen deterministic on Xtensa")]
    #[inline(always)]
    fn xy(&self) -> (i32, i32) {
        (i32::from(self.0), i32::from(self.1))
    }
}

/// [`ring_hit_i64`] generalized over the pair width (monomorphizes to the
/// same loop; i16 pairs widen in the load).
#[cfg(feature = "geom-image")]
pub fn ring_hit_pairs<P: CoordPair>(ring: &[P], px: i32, py: i32) -> RingHit {
    let n = ring.len();
    if n < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[n - 1].xy();
    for p in ring {
        let (x1, y1) = p.xy();
        match edge_i64((x0, y0), (x1, y1), px, py) {
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

/// [`ring_hit_pairs`] at i128 edge width — the i32-quant image path.
#[cfg(feature = "geom-image")]
pub fn ring_hit_pairs_wide<P: CoordPair>(ring: &[P], px: i32, py: i32) -> RingHit {
    let n = ring.len();
    if n < 3 {
        return RingHit::Outside;
    }
    let mut inside = false;
    let (mut x0, mut y0) = ring[n - 1].xy();
    for p in ring {
        let (x1, y1) = p.xy();
        match edge_i128((x0, y0), (x1, y1), px, py) {
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
        assert!(contains_i64(&[SQUARE], 5, 5));
        assert!(!contains_i64(&[SQUARE], 15, 5));
        assert!(!contains_i64(&[SQUARE], -1, -1));
        // boundary claims: edges + vertices
        assert!(contains_i64(&[SQUARE], 0, 5));
        assert!(contains_i64(&[SQUARE], 5, 0));
        assert!(contains_i64(&[SQUARE], 10, 10));
    }

    #[test]
    fn hole_excludes() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        assert!(contains_i64(poly, 1, 1)); // in exterior, outside hole
        assert!(!contains_i64(poly, 5, 5)); // inside hole
        assert!(contains_i64(poly, 3, 5)); // on hole edge -> claimed
    }

    #[test]
    fn i128_matches_i64_in_range() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        for x in -2..13 {
            for y in -2..13 {
                assert_eq!(contains_i64(poly, x, y), contains_i128(poly, x, y), "at ({x},{y})");
            }
        }
    }

    #[test]
    fn f64_matches_i64_in_range() {
        let poly: &[&[(i32, i32)]] = &[SQUARE, HOLE];
        for x in -2..13 {
            for y in -2..13 {
                assert_eq!(contains_i64(poly, x, y), contains_f64(poly, x, y), "at ({x},{y})");
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
                    contains_i64(rings, px, py),
                    contains_f64(rings, px, py),
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
                let ours = contains_i64(rings, px, py);
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
