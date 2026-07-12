//! Open-polyline simplification algorithms (PLAN.md §14.8), shared between the
//! builder (`utz-build`, per-arc topology-aware pass) and the tuning-viewer
//! HTML (compiled to WASM so the browser preview runs the exact code the
//! builder runs — no JS reimplementation drift).
//!
//! All functions take an open polyline, always keep both endpoints, and return
//! ≥ 2 points. Units are the caller's (the builder works in degrees; convert
//! meters with ~111 320 m/deg, areas with its square). The menu:
//!
//! - [`rdp`] — Ramer–Douglas–Peucker (Ramer 1972; Douglas & Peucker 1973):
//!   max perpendicular deviation ≤ ε guaranteed. The default.
//! - [`visvalingam`] — Visvalingam–Whyatt (1993): iteratively drop the point
//!   spanning the smallest triangle. Parameter is an *area*, not a distance —
//!   no ε-style deviation bound, but often a cartographically nicer caricature
//!   at the same vertex budget.
//! - [`imai_iri`] — Imai–Iri (1988): the provably *minimum* number of vertices
//!   for a given deviation bound ε (shortest path over the shortcut graph).
//!   Same guarantee as RDP, fewer-or-equal points, more build time.
//!
//! Corridor/streaming algorithms (Reumann–Witkam, Opheim, Lang, Zhao–Saalfeld)
//! were considered and rejected: they trade quality-per-vertex for single-pass
//! speed, which is worthless at build time (PLAN.md §14.8).
//!
//! Each algorithm also has a weighted variant ([`simplify_weighted`], `*_w`):
//! a per-vertex tolerance multiplier `w[i]` makes the effective parameter
//! `eps * w[i]` (Visvalingam: `min_area * w[i]²`, areas scale as distance²).
//! The builder uses this for population-density-aware refinement — denser
//! areas get smaller multipliers, so boundaries stay precise where people
//! live. `w = 1.0` everywhere reproduces the scalar functions exactly.

#[cfg(target_arch = "wasm32")]
mod wasm;

/// Algorithm + parameter, for callers that thread the choice through knobs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Simplify {
    /// Keep every vertex.
    None,
    /// Ramer–Douglas–Peucker, max deviation `eps`.
    Rdp { eps: f64 },
    /// Visvalingam–Whyatt, drop triangles smaller than `min_area`.
    Visvalingam { min_area: f64 },
    /// Imai–Iri minimum-vertex within max deviation `eps`.
    ImaiIri { eps: f64 },
}

/// Dispatch on [`Simplify`].
#[must_use]
pub fn simplify(algo: Simplify, pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    match algo {
        Simplify::None => pts.to_vec(),
        Simplify::Rdp { eps } => rdp(pts, eps),
        Simplify::Visvalingam { min_area } => visvalingam(pts, min_area),
        Simplify::ImaiIri { eps } => imai_iri(pts, eps),
    }
}

/// [`simplify`] with per-vertex tolerance multipliers: the effective parameter
/// at `pts[i]` is `param * w[i]` (Visvalingam: `min_area * w[i]²`). One
/// strictly positive weight per point; all-ones reproduces [`simplify`].
#[must_use]
pub fn simplify_weighted(algo: Simplify, pts: &[(f64, f64)], w: &[f64]) -> Vec<(f64, f64)> {
    match algo {
        Simplify::None => pts.to_vec(),
        Simplify::Rdp { eps } => rdp_w(pts, eps, w),
        Simplify::Visvalingam { min_area } => visvalingam_w(pts, min_area, w),
        Simplify::ImaiIri { eps } => imai_iri_w(pts, eps, w),
    }
}

/// Population density → tolerance-multiplier map, shared by the builder and
/// the live viewer (compiled into the WASM module so the browser's weighting
/// slider runs the same code). Refine-only: weight is 1 below `d_lo`
/// (oceans, deserts — zero size regression there) and `w_min` above `d_hi`
/// (city cores), log-log linear between the knees:
/// `weight(d) = (d/d_lo)^-k`, `k = ln(1/w_min) / ln(d_hi/d_lo)`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DensityWeight {
    /// multiplier at saturation (0.1 → dense areas get one tenth the eps)
    pub w_min: f64,
    /// density (people/km²) below which the weight stays 1
    pub d_lo: f64,
    /// density above which the weight saturates at `w_min`
    pub d_hi: f64,
}

impl DensityWeight {
    /// `d_lo` default: below ~5 people/km² nobody is misassigned that matters.
    pub const DEFAULT_D_LO: f64 = 5.0;
    /// `d_hi` default: ~2000 people/km² is already a dense city.
    pub const DEFAULT_D_HI: f64 = 2000.0;

    #[must_use]
    pub fn new(w_min: f64) -> Self {
        Self { w_min, d_lo: Self::DEFAULT_D_LO, d_hi: Self::DEFAULT_D_HI }
    }

    /// Tolerance multiplier ∈ [`w_min`, 1] for a density in people/km²
    /// (NaN or `w_min ≥ 1` → 1: weighting off).
    #[must_use]
    pub fn weight(&self, density: f64) -> f64 {
        if density.is_nan() || density <= self.d_lo || self.w_min >= 1.0 {
            return 1.0;
        }
        if density >= self.d_hi {
            return self.w_min;
        }
        let k = self.w_min.recip().ln() / (self.d_hi / self.d_lo).ln();
        (density / self.d_lo).powf(-k).clamp(self.w_min, 1.0)
    }
}

/// Squared distance from `p` to the segment `a`–`b`.
fn seg_dist2(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return (p.0 - a.0).powi(2) + (p.1 - a.1).powi(2);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    (p.0 - cx).powi(2) + (p.1 - cy).powi(2)
}

/// Ramer–Douglas–Peucker keeping both endpoints; result has ≥ 2 points.
#[must_use]
pub fn rdp(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 || eps <= 0.0 {
        return pts.to_vec();
    }
    let e2 = eps * eps;
    let keep = rdp_keep(pts, |_| e2);
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(&p, _)| p).collect()
}

/// [`rdp`] with per-vertex multipliers: deviation at `pts[i]` ≤ `eps * w[i]`.
///
/// # Panics
///
/// Panics if `pts.len() != w.len()`.
#[must_use]
pub fn rdp_w(pts: &[(f64, f64)], eps: f64, w: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(pts.len(), w.len(), "one weight per point");
    if pts.len() < 3 || eps <= 0.0 {
        return pts.to_vec();
    }
    let e2: Vec<f64> = w.iter().map(|&x| (eps * x).powi(2)).collect();
    let keep = rdp_keep(pts, |i| e2[i]);
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(&p, _)| p).collect()
}

/// Keep-mask form of RDP (the Imai–Iri prefilter needs kept points mapped
/// back to original indices). `e2_of(i)` = squared tolerance at `pts[i]`.
fn rdp_keep(pts: &[(f64, f64)], e2_of: impl Fn(usize) -> f64 + Copy) -> Vec<bool> {
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    *keep.last_mut().unwrap() = true;
    rdp_rec(pts, 0, pts.len() - 1, e2_of, &mut keep);
    keep
}

fn rdp_rec(p: &[(f64, f64)], a: usize, b: usize, e2_of: impl Fn(usize) -> f64 + Copy, keep: &mut [bool]) {
    if b <= a + 1 {
        return;
    }
    // farthest point, measured in units of its own tolerance
    let (mut im, mut rm) = (a, 0.0);
    for i in a + 1..b {
        let r = seg_dist2(p[i], p[a], p[b]) / e2_of(i);
        if r > rm {
            rm = r;
            im = i;
        }
    }
    if rm > 1.0 {
        keep[im] = true;
        rdp_rec(p, a, im, e2_of, keep);
        rdp_rec(p, im, b, e2_of, keep);
    }
}

/// Visvalingam–Whyatt: repeatedly remove the interior point whose triangle
/// (prev, point, next) has the smallest area, while that area < `min_area`.
/// Ties break on lower index for reproducible builds.
#[must_use]
pub fn visvalingam(pts: &[(f64, f64)], min_area: f64) -> Vec<(f64, f64)> {
    vw_impl(pts, min_area, |_| 1.0)
}

/// [`visvalingam`] with per-vertex multipliers: `pts[i]` is dropped while its
/// triangle area < `min_area * w[i]²` (areas scale as distance²).
///
/// # Panics
///
/// Panics if `pts.len() != w.len()`.
#[must_use]
pub fn visvalingam_w(pts: &[(f64, f64)], min_area: f64, w: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(pts.len(), w.len(), "one weight per point");
    let w2: Vec<f64> = w.iter().map(|&x| x * x).collect();
    vw_impl(pts, min_area, |i| w2[i])
}

/// [`vw_impl`] heap entry; max-heap ordered by Reverse-style negated
/// comparison so the smallest effective area pops first.
#[derive(PartialEq)]
struct VwEntry {
    area: f64,
    idx: usize,
    stamp: u32,
}
impl Eq for VwEntry {}
impl Ord for VwEntry {
    fn cmp(&self, o: &Self) -> core::cmp::Ordering {
        // BinaryHeap is a max-heap: invert so the smallest area pops first
        o.area.total_cmp(&self.area).then(o.idx.cmp(&self.idx))
    }
}
impl PartialOrd for VwEntry {
    fn partial_cmp(&self, o: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Shared VW core: heap entries hold the *effective* area `tri / w2_of(idx)`
/// compared against the unscaled `min_area` (division by 1.0 is bit-exact, so
/// the scalar path is unchanged).
fn vw_impl(pts: &[(f64, f64)], min_area: f64, w2_of: impl Fn(usize) -> f64 + Copy) -> Vec<(f64, f64)> {
    let n = pts.len();
    if n < 3 || min_area <= 0.0 {
        return pts.to_vec();
    }
    let tri = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> f64 {
        0.5 * ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs()
    };
    let mut prev: Vec<usize> = (0..n).map(|i| i.wrapping_sub(1)).collect();
    let mut next: Vec<usize> = (1..=n).collect();
    let mut alive = vec![true; n];
    let mut stamp = vec![0u32; n];

    let mut heap = std::collections::BinaryHeap::with_capacity(n);
    for i in 1..n - 1 {
        heap.push(VwEntry { area: tri(pts[i - 1], pts[i], pts[i + 1]) / w2_of(i), idx: i, stamp: 0 });
    }
    while let Some(e) = heap.pop() {
        if !alive[e.idx] || e.stamp != stamp[e.idx] {
            continue; // stale entry
        }
        if e.area >= min_area {
            break;
        }
        alive[e.idx] = false;
        let (p, nx) = (prev[e.idx], next[e.idx]);
        next[p] = nx;
        prev[nx] = p;
        for nb in [p, nx] {
            if nb != 0 && nb != n - 1 {
                stamp[nb] += 1;
                heap.push(VwEntry {
                    area: tri(pts[prev[nb]], pts[nb], pts[next[nb]]) / w2_of(nb),
                    idx: nb,
                    stamp: stamp[nb],
                });
            }
        }
    }
    pts.iter().zip(alive).filter(|(_, a)| *a).map(|(&p, _)| p).collect()
}

/// Imai–Iri: the minimum-vertex polyline whose deviation from `pts` is ≤ `eps`
/// — BFS for the fewest hops from first to last point over the graph of
/// "shortcut" segments that stay within `eps` of every skipped point.
///
/// The exact core is ~O(n²) (Chan–Chin wedges, O(1) amortized per shortcut
/// check); very long inputs are RDP prefiltered with an *adaptive* slice of
/// the budget — start at `eps/10` (Imai–Iri's vertex count is driven by its
/// share, so give it as much as possible: an even split kept MORE points than
/// plain RDP on real arcs) and escalate toward the `eps/2` cap only while the
/// prefiltered arc stays too big. Deviation bounds compose, so the total
/// stays ≤ `eps`; prefiltered results are near-optimal rather than optimal.
#[must_use]
pub fn imai_iri(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 || eps <= 0.0 {
        return pts.to_vec();
    }
    if pts.len() <= II_MAX {
        return imai_iri_core(pts, |_| eps);
    }
    let mut pre_eps = eps * 0.1;
    loop {
        let pre = rdp(pts, pre_eps);
        if pre.len() <= 2 * II_MAX || pre_eps >= eps * 0.5 {
            let rest = eps - pre_eps;
            return imai_iri_core(&pre, |_| rest);
        }
        pre_eps = (pre_eps * 2.0).min(eps * 0.5);
    }
}

/// caps the lazy backward-row bitsets at `2·II_MAX` ≈ 32 MB transient
const II_MAX: usize = 8192;

/// [`imai_iri`] with per-vertex multipliers: deviation at `pts[i]` ≤
/// `eps * w[i]`. Above `II_MAX` points the RDP prefilter composes per point, so
/// that bound is exact only where `w` is locally ~constant across a prefilter
/// shortcut (negligible for weights sampled from a coarse grid through a
/// smooth map); the global `eps * max(w)` bound always holds.
///
/// # Panics
///
/// Panics if `pts.len() != w.len()`.
#[must_use]
pub fn imai_iri_w(pts: &[(f64, f64)], eps: f64, w: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(pts.len(), w.len(), "one weight per point");
    if pts.len() < 3 || eps <= 0.0 {
        return pts.to_vec();
    }
    if pts.len() <= II_MAX {
        return imai_iri_core(pts, |i| eps * w[i]);
    }
    // mirrors the scalar adaptive prefilter arithmetic exactly, so all-ones
    // weights stay bit-identical to imai_iri
    let mut pre_eps = eps * 0.1;
    loop {
        let keep = rdp_keep(pts, |i| (pre_eps * w[i]).powi(2));
        let kept = keep.iter().filter(|k| **k).count();
        if kept <= 2 * II_MAX || pre_eps >= eps * 0.5 {
            let idx: Vec<usize> =
                keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect();
            let pre: Vec<(f64, f64)> = idx.iter().map(|&i| pts[i]).collect();
            let rest = eps - pre_eps;
            return imai_iri_core(&pre, |m| rest * w[idx[m]]);
        }
        pre_eps = (pre_eps * 2.0).min(eps * 0.5);
    }
}

/// Angular interval of directions (≤ π wide, shrinks under intersection):
/// `u` is inside iff `cross(lo, u) ≥ 0 ∧ cross(u, hi) ≥ 0`.
struct Wedge {
    lo: (f64, f64),
    hi: (f64, f64),
    any: bool,   // no constraint yet — full circle
    empty: bool, // intersection pinched shut — nothing valid beyond here
}

fn cross(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.1 - a.1 * b.0
}

impl Wedge {
    fn new() -> Self {
        Wedge { lo: (0.0, 0.0), hi: (0.0, 0.0), any: true, empty: false }
    }
    /// Intersect with the wedge of directions whose ray from the anchor
    /// passes within ε of a point at unit direction `c`, `sin_phi` = ε/dist.
    fn add(&mut self, c: (f64, f64), sin_phi: f64) {
        let cos_phi = (1.0 - sin_phi * sin_phi).max(0.0).sqrt();
        let lo = (c.0 * cos_phi + c.1 * sin_phi, -c.0 * sin_phi + c.1 * cos_phi);
        let hi = (c.0 * cos_phi - c.1 * sin_phi, c.0 * sin_phi + c.1 * cos_phi);
        if self.any {
            (self.lo, self.hi, self.any) = (lo, hi, false);
            return;
        }
        // Two arcs each ≤ π wide intersect iff an endpoint of one lies inside
        // the other; when they do, the intersection is [ccw-most start,
        // cw-most end] by MEMBERSHIP, not by pairwise cross comparison — a
        // disjoint interval > 180° away otherwise slips through unchanged.
        let in_cur = |u: (f64, f64)| cross(self.lo, u) >= 0.0 && cross(u, self.hi) >= 0.0;
        let in_new = |u: (f64, f64)| cross(lo, u) >= 0.0 && cross(u, hi) >= 0.0;
        let (lo_in, hi_in) = (in_cur(lo), in_cur(hi));
        if !(lo_in || hi_in || in_new(self.lo)) {
            self.empty = true;
            return;
        }
        if lo_in {
            self.lo = lo;
        }
        if hi_in {
            self.hi = hi;
        }
        // float-slop safety: a hairline wedge may come out inverted — treat
        // as empty (rejects a valid shortcut at worst, never accepts a bad one)
        if cross(self.lo, self.hi) < 0.0 {
            self.empty = true;
        }
    }
    fn contains(&self, d: (f64, f64)) -> bool {
        !self.empty && (self.any || (cross(self.lo, d) >= 0.0 && cross(d, self.hi) >= 0.0))
    }
}

/// Ray-validity sweep from `pts[from]`: walk `ks` (the intermediate points in
/// sweep order), calling `visit(k_target, ok)` where `ok` ⟺ the ray from
/// `pts[from]` toward `pts[k_target]` stays within ε of every point already
/// swept. `dist(p_k, seg(i,j)) ≤ ε_k ⟺ ray-from-i ok ∧ ray-from-j ok`, so two
/// sweeps decide segment validity exactly. The Chan–Chin lemma is pointwise
/// in `k`, so each point may carry its own tolerance `eps_of(k)`.
fn ray_sweep(pts: &[(f64, f64)], from: usize, ks: impl Iterator<Item = usize>, eps_of: impl Fn(usize) -> f64 + Copy, mut visit: impl FnMut(usize, bool) -> bool) {
    let p0 = pts[from];
    let mut w = Wedge::new();
    let mut has_far = false; // some swept point is > its ε from the anchor
    for k in ks {
        let d = (pts[k].0 - p0.0, pts[k].1 - p0.1);
        let ok = if d == (0.0, 0.0) { !has_far } else { w.contains(d) };
        if !visit(k, ok) {
            return;
        }
        // fold k into the constraints for the points swept after it
        let dist = (d.0 * d.0 + d.1 * d.1).sqrt();
        let eps_k = eps_of(k);
        if dist > eps_k {
            has_far = true;
            w.add((d.0 / dist, d.1 / dist), eps_k / dist);
            if w.empty {
                // constraints only accumulate — nothing further can be valid
                while visit(usize::MAX, false) {}
                return;
            }
        }
    }
}

fn imai_iri_core(pts: &[(f64, f64)], eps_of: impl Fn(usize) -> f64 + Copy) -> Vec<(f64, f64)> {
    let n = pts.len();
    let words = n.div_ceil(64);
    // lazily computed backward rows: bit i of row j ⟺ ray from p_j toward
    // p_i stays within ε of every point strictly between i and j
    let mut bwd: Vec<Option<Vec<u64>>> = vec![None; n];
    let bwd_row = |j: usize| -> Vec<u64> {
        let mut bits = vec![0u64; words];
        ray_sweep(pts, j, (0..j).rev(), eps_of, |i, ok| {
            if i != usize::MAX && ok {
                bits[i / 64] |= 1 << (i % 64);
            }
            i != usize::MAX && i > 0
        });
        bits
    };

    // BFS level by level: first time a node is reached = fewest hops
    let mut parent = vec![usize::MAX; n];
    let mut frontier = vec![0usize];
    parent[0] = 0;
    'bfs: while !frontier.is_empty() {
        let mut nextf = Vec::new();
        for &i in &frontier {
            let mut done = false;
            ray_sweep(pts, i, i + 1..n, eps_of, |j, fwd_ok| {
                if j == usize::MAX {
                    return false; // wedge pinched shut — stop this sweep
                }
                if fwd_ok && parent[j] == usize::MAX {
                    let row = bwd[j].get_or_insert_with(|| bwd_row(j));
                    if row[i / 64] >> (i % 64) & 1 == 1 {
                        parent[j] = i;
                        if j == n - 1 {
                            done = true;
                            return false;
                        }
                        nextf.push(j);
                    }
                }
                true
            });
            if done {
                break 'bfs;
            }
        }
        frontier = nextf;
    }
    let mut path = vec![n - 1];
    while *path.last().unwrap() != 0 {
        path.push(parent[*path.last().unwrap()]);
    }
    path.iter().rev().map(|&i| pts[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// deterministic pseudo-random polyline (LCG, same recipe as pip tests)
    fn wiggle(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut lcg = utz_common::Lcg::new(seed);
        (0..n).map(#[expect(clippy::cast_precision_loss, reason = "i < n ≤ II_MAX+2000 ≪ 2^53; test x-spacing")] |i| (i as f64 * 0.1, lcg.unit_f64() * 2.0 - 1.0)).collect()
    }

    fn max_deviation(orig: &[(f64, f64)], simp: &[(f64, f64)]) -> f64 {
        // every original point must be near some simplified segment
        orig.iter()
            .map(|&p| {
                simp.windows(2)
                    .map(|w| seg_dist2(p, w[0], w[1]))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max)
            .sqrt()
    }

    #[test]
    fn endpoints_always_kept() {
        let pts = wiggle(50, 1);
        for out in [rdp(&pts, 0.5), visvalingam(&pts, 0.5), imai_iri(&pts, 0.5)] {
            assert_eq!(out.first(), pts.first());
            assert_eq!(out.last(), pts.last());
            assert!(out.len() >= 2);
        }
    }

    #[test]
    fn collinear_collapses() {
        let line: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), 0.0)).collect();
        assert_eq!(rdp(&line, 0.01).len(), 2);
        assert_eq!(visvalingam(&line, 0.01).len(), 2);
        assert_eq!(imai_iri(&line, 0.01).len(), 2);
    }

    #[test]
    fn spike_survives() {
        let mut line: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), 0.0)).collect();
        line[5] = (5.0, 3.0);
        assert!(rdp(&line, 0.5).contains(&(5.0, 3.0)));
        assert!(visvalingam(&line, 0.5).contains(&(5.0, 3.0)));
        // imai_iri may route around the spike's neighbours but must stay in bound
        assert!(max_deviation(&line, &imai_iri(&line, 0.5)) <= 0.5 + 1e-12);
    }

    #[test]
    fn eps_bound_honored() {
        for seed in 1..=20u64 {
            let pts = wiggle(200, seed);
            for eps in [0.05, 0.2, 0.8] {
                assert!(max_deviation(&pts, &rdp(&pts, eps)) <= eps + 1e-12, "rdp seed {seed}");
                assert!(max_deviation(&pts, &imai_iri(&pts, eps)) <= eps + 1e-12, "ii seed {seed}");
            }
        }
    }

    #[test]
    fn imai_iri_never_more_verts_than_rdp() {
        for seed in 1..=20u64 {
            let pts = wiggle(200, seed);
            for eps in [0.05, 0.2, 0.8] {
                let (r, ii) = (rdp(&pts, eps).len(), imai_iri(&pts, eps).len());
                assert!(ii <= r, "seed {seed} eps {eps}: imai-iri {ii} > rdp {r}");
            }
        }
    }

    #[test]
    fn imai_iri_optimal_vs_bruteforce() {
        // exhaustively check minimality on small inputs
        fn brute_min(pts: &[(f64, f64)], e2: f64) -> usize {
            let n = pts.len();
            // BFS is the definition of minimal; validate against subset search
            for keep in 2..=n {
                // try all interior subsets of size keep-2
                fn combos(n: usize, k: usize) -> Vec<Vec<usize>> {
                    if k == 0 {
                        return vec![vec![]];
                    }
                    let mut out = Vec::new();
                    for first in 1..n - 1 {
                        for mut rest in combos(n, k - 1) {
                            if rest.first().is_none_or(|&r| r > first) {
                                rest.insert(0, first);
                                out.push(rest);
                            }
                        }
                    }
                    out
                }
                for interior in combos(n, keep - 2) {
                    let mut idx = vec![0];
                    idx.extend(interior);
                    idx.push(n - 1);
                    let ok = idx.windows(2).all(|w| {
                        pts[w[0] + 1..w[1]].iter().all(|&p| seg_dist2(p, pts[w[0]], pts[w[1]]) <= e2)
                    });
                    if ok {
                        return keep;
                    }
                }
            }
            n
        }
        for seed in 1..=10u64 {
            let pts = wiggle(9, seed);
            for eps in [0.1, 0.4, 1.0] {
                let ii = imai_iri(&pts, eps).len();
                let opt = brute_min(&pts, eps * eps);
                assert_eq!(ii, opt, "seed {seed} eps {eps}");
            }
        }
    }

    #[test]
    fn visvalingam_monotone_in_threshold() {
        let pts = wiggle(200, 7);
        let mut last = usize::MAX;
        for a in [0.001, 0.01, 0.1, 1.0] {
            let n = visvalingam(&pts, a).len();
            assert!(n <= last);
            last = n;
        }
    }

    #[test]
    fn dispatch_matches_direct() {
        let pts = wiggle(100, 3);
        assert_eq!(simplify(Simplify::None, &pts), pts);
        assert_eq!(simplify(Simplify::Rdp { eps: 0.2 }, &pts), rdp(&pts, 0.2));
        assert_eq!(
            simplify(Simplify::Visvalingam { min_area: 0.2 }, &pts),
            visvalingam(&pts, 0.2)
        );
        assert_eq!(simplify(Simplify::ImaiIri { eps: 0.2 }, &pts), imai_iri(&pts, 0.2));
    }

    /// naive O(n) per-check BFS — the reference the wedge core must match
    fn imai_iri_naive(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
        let n = pts.len();
        let e2 = eps * eps;
        let valid =
            |i: usize, j: usize| pts[i + 1..j].iter().all(|&p| seg_dist2(p, pts[i], pts[j]) <= e2);
        let mut parent = vec![usize::MAX; n];
        let mut frontier = vec![0usize];
        parent[0] = 0;
        'bfs: while !frontier.is_empty() {
            let mut nextf = Vec::new();
            for &i in &frontier {
                for (j, pj) in parent.iter_mut().enumerate().skip(i + 1) {
                    if *pj == usize::MAX && valid(i, j) {
                        *pj = i;
                        if j == n - 1 {
                            break 'bfs;
                        }
                        nextf.push(j);
                    }
                }
            }
            frontier = nextf;
        }
        let mut path = vec![n - 1];
        while *path.last().unwrap() != 0 {
            path.push(parent[*path.last().unwrap()]);
        }
        path.iter().rev().map(|&i| pts[i]).collect()
    }

    #[test]
    fn wedge_core_matches_naive() {
        // the disjoint-interval wraparound bug needed n ≈ thousands to
        // surface — cover both small and large inputs
        for seed in 1..=30u64 {
            let pts = wiggle(400, seed);
            for eps in [0.03, 0.1, 0.3, 0.9] {
                let (w, nv) =
                    (imai_iri_core(&pts, |_| eps).len(), imai_iri_naive(&pts, eps).len());
                assert_eq!(w, nv, "seed {seed} eps {eps}: wedge {w} != naive {nv}");
            }
        }
        for seed in 1..=3u64 {
            let pts = wiggle(3000, seed);
            for eps in [0.3, 0.9] {
                let (w, nv) =
                    (imai_iri_core(&pts, |_| eps).len(), imai_iri_naive(&pts, eps).len());
                assert_eq!(w, nv, "seed {seed} eps {eps} (n=3000): wedge {w} != naive {nv}");
            }
        }
    }

    #[test]
    fn long_arc_prefilter_keeps_bound() {
        let pts = wiggle(II_MAX + 2000, 11); // > II_MAX → adaptive rdp prefilter + core
        let eps = 0.3;
        let out = imai_iri(&pts, eps);
        assert!(max_deviation(&pts, &out) <= eps + 1e-12);
        assert!(out.len() < pts.len());
    }

    /// distance from one original point to the simplified polyline
    fn point_dev(p: (f64, f64), simp: &[(f64, f64)]) -> f64 {
        simp.windows(2).map(|w| seg_dist2(p, w[0], w[1])).fold(f64::INFINITY, f64::min).sqrt()
    }

    /// deterministic pseudo-random weights in [lo, 1]
    fn rand_weights(n: usize, lo: f64, seed: u64) -> Vec<f64> {
        let mut lcg = utz_common::Lcg::new(seed);
        (0..n).map(|_| lo + (1.0 - lo) * lcg.unit_f64()).collect()
    }

    #[test]
    fn weighted_uniform_matches_scalar() {
        for seed in 1..=10u64 {
            let pts = wiggle(300, seed);
            let ones = vec![1.0; pts.len()];
            for eps in [0.05, 0.2, 0.8] {
                assert_eq!(rdp_w(&pts, eps, &ones), rdp(&pts, eps), "rdp seed {seed}");
                assert_eq!(
                    visvalingam_w(&pts, eps, &ones),
                    visvalingam(&pts, eps),
                    "vw seed {seed}"
                );
                assert_eq!(imai_iri_w(&pts, eps, &ones), imai_iri(&pts, eps), "ii seed {seed}");
            }
        }
        // prefilter path: the weighted adaptive loop mirrors the scalar one
        let pts = wiggle(II_MAX + 2000, 5);
        let ones = vec![1.0; pts.len()];
        assert_eq!(imai_iri_w(&pts, 0.3, &ones), imai_iri(&pts, 0.3));
    }

    #[test]
    fn weighted_per_point_bound() {
        // n ≤ II_MAX so the imai-iri bound is exact (no prefilter composition)
        for seed in 1..=10u64 {
            let pts = wiggle(300, seed);
            let w = rand_weights(pts.len(), 0.1, seed);
            let eps = 0.4;
            for out in [rdp_w(&pts, eps, &w), imai_iri_w(&pts, eps, &w)] {
                for (i, &p) in pts.iter().enumerate() {
                    let (dev, bound) = (point_dev(p, &out), eps * w[i]);
                    assert!(dev <= bound + 1e-12, "seed {seed} pt {i}: {dev} > {bound}");
                }
            }
        }
    }

    #[test]
    fn weighted_refines_locally() {
        // a bump under the uniform tolerance vanishes — until its weight drops
        let mut line: Vec<(f64, f64)> = (0..10).map(|i| (f64::from(i), 0.0)).collect();
        line[5] = (5.0, 0.3);
        let bump = line[5];
        let mut w = vec![1.0; line.len()];
        w[5] = 0.1;
        assert!(!rdp(&line, 0.5).contains(&bump));
        assert!(rdp_w(&line, 0.5, &w).contains(&bump));
        assert!(!visvalingam(&line, 0.5).contains(&bump));
        assert!(visvalingam_w(&line, 0.5, &w).contains(&bump));
        // every imai-iri shortcut spanning the bump misses it by 0.3 > 0.05,
        // so the bump itself must be a shortcut endpoint
        assert!(!imai_iri(&line, 0.5).contains(&bump));
        assert!(imai_iri_w(&line, 0.5, &w).contains(&bump));
    }

    #[test]
    fn weighted_long_arc_prefilter_bound() {
        // > II_MAX: per-point bound may relax where w varies within a
        // prefilter shortcut, but the global eps·max(w) bound always holds
        let pts = wiggle(II_MAX + 2000, 13);
        #[expect(clippy::cast_precision_loss, reason = "i < pts.len() = II_MAX+2000 ≪ 2^53; test weights")]
        let w: Vec<f64> = (0..pts.len()).map(|i| 0.6 + 0.4 * (i as f64 / 500.0).sin()).collect();
        let eps = 0.3;
        let out = imai_iri_w(&pts, eps, &w);
        assert!(out.len() < pts.len());
        for &p in &pts {
            assert!(point_dev(p, &out) <= eps + 1e-12);
        }
    }

    #[test]
    fn weighted_dispatch_matches_direct() {
        let pts = wiggle(100, 3);
        let w = rand_weights(pts.len(), 0.2, 9);
        assert_eq!(simplify_weighted(Simplify::None, &pts, &w), pts);
        assert_eq!(simplify_weighted(Simplify::Rdp { eps: 0.2 }, &pts, &w), rdp_w(&pts, 0.2, &w));
        assert_eq!(
            simplify_weighted(Simplify::Visvalingam { min_area: 0.2 }, &pts, &w),
            visvalingam_w(&pts, 0.2, &w)
        );
        assert_eq!(
            simplify_weighted(Simplify::ImaiIri { eps: 0.2 }, &pts, &w),
            imai_iri_w(&pts, 0.2, &w)
        );
    }

    #[test]
    fn density_weight_shape() {
        let m = DensityWeight::new(0.1);
        assert_eq!(m.weight(0.0), 1.0);
        assert_eq!(m.weight(m.d_lo), 1.0);
        assert_eq!(m.weight(1e12), 0.1);
        assert_eq!(m.weight(f64::NAN), 1.0);
        // continuous at both knees, monotone nonincreasing between
        assert!((m.weight(m.d_lo * 1.0001) - 1.0).abs() < 1e-3);
        assert!((m.weight(m.d_hi * 0.9999) - 0.1).abs() < 1e-3);
        let mut last = 1.0;
        for i in 0..100 {
            let d = m.d_lo * (m.d_hi / m.d_lo).powf(f64::from(i) / 99.0);
            let w = m.weight(d);
            assert!(w <= last + 1e-15 && (m.w_min..=1.0).contains(&w), "d={d} w={w}");
            last = w;
        }
        // weighting off
        assert_eq!(DensityWeight::new(1.0).weight(1e9), 1.0);
    }
}
