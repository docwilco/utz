//! Open-polyline simplification algorithms, shared between the builder
//! (`utz_build`, whose per-arc pass is topology-aware) and the tuning-viewer
//! HTML (compiled to WASM so the browser preview runs the exact code the
//! builder runs, with no JS reimplementation drift).
//!
//! All functions take an open polyline, always keep both endpoints, and return
//! ≥ 2 points. Units are the caller's (the builder works in degrees; convert
//! meters with ~111 320 m/deg and areas with its square). Three algorithms are
//! on the menu:
//!
//! - [`rdp()`] is Ramer–Douglas–Peucker (Ramer 1972; Douglas & Peucker 1973),
//!   which guarantees a max perpendicular deviation ≤ ε. It is the default.
//! - [`visvalingam()`] is Visvalingam–Whyatt (1993), which iteratively drops
//!   the point spanning the smallest triangle. Its parameter is an *area*, not
//!   a distance: there is no ε-style deviation bound, but the result is often
//!   a cartographically nicer caricature at the same vertex budget.
//! - [`imai_iri()`] is Imai–Iri (1988), which finds the provably *minimum*
//!   number of vertices for a given deviation bound ε (the shortest path over
//!   the shortcut graph). It gives the same guarantee as RDP with
//!   fewer-or-equal points and more build time.
//!
//! Corridor/streaming algorithms (Reumann–Witkam, Opheim, Lang, Zhao–Saalfeld)
//! were considered and rejected: they trade quality-per-vertex for single-pass
//! speed, which is worthless at build time.
//!
//! Each algorithm also has a weighted variant ([`simplify_weighted()`],
//! `*_w`): a per-vertex tolerance multiplier `weights[i]` makes the effective
//! parameter `epsilon * weights[i]` (for Visvalingam it is
//! `min_area * weights[i]²`, because areas scale as distance²). The builder
//! uses this for population-density-aware refinement: denser areas get smaller
//! multipliers, so boundaries stay precise where people live.
//! `weights[i] = 1.0` everywhere reproduces the scalar functions exactly.
//!
//! [`rdp()`]: ../utz_simplify/fn.rdp.html
//! [`visvalingam()`]: ../utz_simplify/fn.visvalingam.html
//! [`imai_iri()`]: ../utz_simplify/fn.imai_iri.html
//! [`simplify_weighted()`]: ../utz_simplify/fn.simplify_weighted.html

#[cfg(target_arch = "wasm32")]
mod wasm;

/// The algorithm choice and its parameter, for callers that thread the choice
/// through knobs.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Simplify {
    /// Keeps every vertex.
    None,
    /// Ramer–Douglas–Peucker with a max deviation of `epsilon`.
    Rdp { epsilon: f64 },
    /// Visvalingam–Whyatt, which drops triangles smaller than `min_area`.
    Visvalingam { min_area: f64 },
    /// Imai–Iri minimum-vertex simplification within a max deviation of
    /// `epsilon`.
    ImaiIri { epsilon: f64 },
}

/// Dispatches on [`Simplify`].
#[must_use]
pub fn simplify(algorithm: Simplify, points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    match algorithm {
        Simplify::None => points.to_vec(),
        Simplify::Rdp { epsilon } => rdp(points, epsilon),
        Simplify::Visvalingam { min_area } => visvalingam(points, min_area),
        Simplify::ImaiIri { epsilon } => imai_iri(points, epsilon),
    }
}

/// [`simplify()`] with per-vertex tolerance multipliers: the effective
/// parameter at `points[i]` is `param * weights[i]` (for Visvalingam,
/// `min_area * weights[i]²`). Callers pass one strictly positive weight per
/// point; all-ones weights reproduce [`simplify()`].
///
/// # Panics
///
/// Panics if `points.len() != weights.len()`.
#[must_use]
pub fn simplify_weighted(
    algorithm: Simplify,
    points: &[(f64, f64)],
    weights: &[f64],
) -> Vec<(f64, f64)> {
    assert_eq!(points.len(), weights.len(), "one weight per point");
    match algorithm {
        Simplify::None => points.to_vec(),
        Simplify::Rdp { epsilon } => rdp_w(points, epsilon, weights),
        Simplify::Visvalingam { min_area } => visvalingam_w(points, min_area, weights),
        Simplify::ImaiIri { epsilon } => imai_iri_w(points, epsilon, weights),
    }
}

/// The map from population density to tolerance multiplier: the
/// [`weight()`](Self::weight) method takes a density in people/km² and
/// returns the multiplier to apply to the simplification tolerance there.
/// The map is shared by the builder and the live viewer (it is compiled
/// into the WASM module so the browser's weighting slider runs the same
/// code).
///
/// [`weight()`](Self::weight) evaluates a piecewise curve over density `d`,
/// shaped by two knees (`d_lo`, `d_hi`) and a floor (`w_min`):
///
/// - Below `d_lo` the weight is 1, leaving the baseline tolerance
///   untouched. Oceans, deserts, and other empty areas therefore simplify
///   exactly as they would unweighted, so enabling weighting causes zero
///   size regression there.
/// - Above `d_hi` the weight saturates at `w_min`, giving city cores the
///   finest tolerance the model allows (with `w_min = 0.1`, one tenth of
///   the baseline).
/// - Between the knees the curve is the power law
///   `weight(d) = (d / d_lo)^-k`, which is a straight line in log-log
///   space. The exponent `k = ln(1/w_min) / ln(d_hi/d_lo)` is the unique
///   one that meets both knees continuously. A power law fits because
///   population density spans orders of magnitude: each doubling of
///   density tightens the weight by the same factor, rather than spending
///   the whole ramp on the first sliver of the range as a linear
///   interpolation would.
///
/// Every weight is ≤ 1, so the map only ever refines: weighted
/// simplification keeps at least the boundary detail of the unweighted
/// baseline and adds more where people live.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DensityWeight {
    /// The multiplier at saturation (0.1 gives dense areas one tenth the
    /// epsilon).
    pub w_min: f64,
    /// The density (people/km²) below which the weight stays 1.
    pub d_lo: f64,
    /// The density above which the weight saturates at `w_min`.
    pub d_hi: f64,
}

impl DensityWeight {
    /// The default for `d_lo`: below ~5 people/km², nobody is misassigned
    /// that matters.
    pub const DEFAULT_D_LO: f64 = 5.0;
    /// The default for `d_hi`: ~2000 people/km² is already a dense city.
    pub const DEFAULT_D_HI: f64 = 2000.0;

    #[must_use]
    pub fn new(w_min: f64) -> Self {
        Self {
            w_min,
            d_lo: Self::DEFAULT_D_LO,
            d_hi: Self::DEFAULT_D_HI,
        }
    }

    /// Returns the tolerance multiplier ∈ [`w_min`, 1] for a density in
    /// people/km². A NaN density or `w_min ≥ 1` returns 1, which turns
    /// weighting off.
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

/// The squared distance from `p` to the segment `a`–`b`.
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

/// Runs Ramer–Douglas–Peucker, keeping both endpoints; the result has
/// ≥ 2 points.
#[must_use]
pub fn rdp(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 || epsilon <= 0.0 {
        return points.to_vec();
    }
    let e2 = epsilon * epsilon;
    let keep = rdp_keep(points, |_| e2);
    points
        .iter()
        .zip(keep)
        .filter_map(|(&point, kept)| kept.then_some(point))
        .collect()
}

/// [`rdp()`] with per-vertex multipliers: the deviation at `points[i]` stays
/// ≤ `epsilon * weights[i]`.
///
/// # Panics
///
/// Panics if `points.len() != weights.len()`.
#[must_use]
pub fn rdp_w(points: &[(f64, f64)], epsilon: f64, weights: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(points.len(), weights.len(), "one weight per point");
    if points.len() < 3 || epsilon <= 0.0 {
        return points.to_vec();
    }
    let e2: Vec<f64> = weights
        .iter()
        .map(|&weight| (epsilon * weight).powi(2))
        .collect();
    let keep = rdp_keep(points, |i| e2[i]);
    points
        .iter()
        .zip(keep)
        .filter_map(|(&point, kept)| kept.then_some(point))
        .collect()
}

/// The keep-mask form of RDP (the Imai–Iri prefilter needs kept points mapped
/// back to original indices). `e2_of(i)` gives the squared tolerance at
/// `points[i]`.
fn rdp_keep(points: &[(f64, f64)], e2_of: impl Fn(usize) -> f64 + Copy) -> Vec<bool> {
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    *keep.last_mut().unwrap() = true;
    rdp_rec(points, 0, points.len() - 1, e2_of, &mut keep);
    keep
}

fn rdp_rec(
    points: &[(f64, f64)],
    start: usize,
    end: usize,
    e2_of: impl Fn(usize) -> f64 + Copy,
    keep: &mut [bool],
) {
    if end <= start + 1 {
        return;
    }
    // farthest point, measured in units of its own tolerance
    let (mut farthest_index, mut farthest_ratio) = (start, 0.0);
    for i in start + 1..end {
        let ratio = seg_dist2(points[i], points[start], points[end]) / e2_of(i);
        if ratio > farthest_ratio {
            farthest_ratio = ratio;
            farthest_index = i;
        }
    }
    if farthest_ratio > 1.0 {
        keep[farthest_index] = true;
        rdp_rec(points, start, farthest_index, e2_of, keep);
        rdp_rec(points, farthest_index, end, e2_of, keep);
    }
}

/// Runs Visvalingam–Whyatt: it repeatedly removes the interior point whose
/// triangle (prev, point, next) has the smallest area, while that area is
/// < `min_area`. Ties break on the lower index for reproducible builds.
#[must_use]
pub fn visvalingam(points: &[(f64, f64)], min_area: f64) -> Vec<(f64, f64)> {
    vw_impl(points, min_area, |_| 1.0)
}

/// [`visvalingam()`] with per-vertex multipliers: `points[i]` is dropped while
/// its triangle area is < `min_area * weights[i]²` (areas scale as
/// distance²).
///
/// # Panics
///
/// Panics if `points.len() != weights.len()`.
#[must_use]
pub fn visvalingam_w(points: &[(f64, f64)], min_area: f64, weights: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(points.len(), weights.len(), "one weight per point");
    let w2: Vec<f64> = weights.iter().map(|&weight| weight * weight).collect();
    vw_impl(points, min_area, |i| w2[i])
}

/// A heap entry for [`vw_impl()`]; the max-heap is ordered by a Reverse-style
/// negated comparison so the smallest effective area pops first.
#[derive(PartialEq)]
struct VwEntry {
    area: f64,
    index: usize,
    stamp: u32,
}
impl Eq for VwEntry {}
impl Ord for VwEntry {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // BinaryHeap is a max-heap: invert so the smallest area pops first
        other
            .area
            .total_cmp(&self.area)
            .then(other.index.cmp(&self.index))
    }
}
impl PartialOrd for VwEntry {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The shared VW core: heap entries hold the *effective* area
/// `triangle_area / w2_of(index)`, compared against the unscaled `min_area`
/// (division by 1.0 is bit-exact, so the scalar path is unchanged).
fn vw_impl(
    points: &[(f64, f64)],
    min_area: f64,
    w2_of: impl Fn(usize) -> f64 + Copy,
) -> Vec<(f64, f64)> {
    let n = points.len();
    if n < 3 || min_area <= 0.0 {
        return points.to_vec();
    }
    let triangle_area = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> f64 {
        0.5 * ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs()
    };
    let mut previous: Vec<usize> = (0..n).map(|i| i.wrapping_sub(1)).collect();
    let mut next: Vec<usize> = (1..=n).collect();
    let mut alive = vec![true; n];
    let mut stamp = vec![0u32; n];

    let mut heap = std::collections::BinaryHeap::with_capacity(n);
    for i in 1..n - 1 {
        heap.push(VwEntry {
            area: triangle_area(points[i - 1], points[i], points[i + 1]) / w2_of(i),
            index: i,
            stamp: 0,
        });
    }
    while let Some(entry) = heap.pop() {
        if !alive[entry.index] || entry.stamp != stamp[entry.index] {
            continue; // stale entry
        }
        if entry.area >= min_area {
            break;
        }
        alive[entry.index] = false;
        let (previous_index, next_index) = (previous[entry.index], next[entry.index]);
        next[previous_index] = next_index;
        previous[next_index] = previous_index;
        for neighbor in [previous_index, next_index] {
            if neighbor != 0 && neighbor != n - 1 {
                stamp[neighbor] += 1;
                heap.push(VwEntry {
                    area: triangle_area(
                        points[previous[neighbor]],
                        points[neighbor],
                        points[next[neighbor]],
                    ) / w2_of(neighbor),
                    index: neighbor,
                    stamp: stamp[neighbor],
                });
            }
        }
    }
    points
        .iter()
        .zip(alive)
        .filter_map(|(&point, is_alive)| is_alive.then_some(point))
        .collect()
}

/// Runs Imai–Iri: it returns the minimum-vertex polyline whose deviation from
/// `points` is ≤ `epsilon` (a BFS finds the fewest hops from the first to the last
/// point over the graph of "shortcut" segments that stay within `epsilon` of
/// every skipped point).
///
/// The exact core is ~O(n²) (Chan–Chin wedges, O(1) amortized per shortcut
/// check); very long inputs are RDP prefiltered with an *adaptive* slice of
/// the budget: the prefilter starts at `epsilon/10` (Imai–Iri's vertex count is
/// driven by its share, so it gets as much as possible: an even split kept
/// MORE points than plain RDP on real arcs) and escalates toward the `epsilon/2`
/// cap only while the prefiltered arc stays too big. Deviation bounds
/// compose, so the total stays ≤ `epsilon`; prefiltered results are near-optimal
/// rather than optimal.
#[must_use]
pub fn imai_iri(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    if points.len() < 3 || epsilon <= 0.0 {
        return points.to_vec();
    }
    if points.len() <= II_MAX {
        return imai_iri_core(points, |_| epsilon);
    }
    let mut pre_epsilon = epsilon * 0.1;
    loop {
        let prefiltered = rdp(points, pre_epsilon);
        if prefiltered.len() <= 2 * II_MAX || pre_epsilon >= epsilon * 0.5 {
            let rest = epsilon - pre_epsilon;
            return imai_iri_core(&prefiltered, |_| rest);
        }
        pre_epsilon = (pre_epsilon * 2.0).min(epsilon * 0.5);
    }
}

/// Caps the lazy backward-row bitsets at `2·II_MAX` points, ≈ 32 MB of
/// transient memory.
const II_MAX: usize = 8192;

/// [`imai_iri()`] with per-vertex multipliers: the deviation at `points[i]`
/// stays ≤ `epsilon * weights[i]`. Above `II_MAX` points the RDP prefilter
/// composes per point, so that bound is exact only where `weights` is locally
/// ~constant across a prefilter shortcut (a negligible caveat for weights
/// sampled from a coarse grid through a smooth map); the global
/// `epsilon * max(weights)` bound always holds.
///
/// # Panics
///
/// Panics if `points.len() != weights.len()`.
#[must_use]
pub fn imai_iri_w(points: &[(f64, f64)], epsilon: f64, weights: &[f64]) -> Vec<(f64, f64)> {
    assert_eq!(points.len(), weights.len(), "one weight per point");
    if points.len() < 3 || epsilon <= 0.0 {
        return points.to_vec();
    }
    if points.len() <= II_MAX {
        return imai_iri_core(points, |i| epsilon * weights[i]);
    }
    // mirrors the scalar adaptive prefilter arithmetic exactly, so all-ones
    // weights stay bit-identical to imai_iri
    let mut pre_epsilon = epsilon * 0.1;
    loop {
        let keep = rdp_keep(points, |i| (pre_epsilon * weights[i]).powi(2));
        let kept_count = keep.iter().filter(|keep_flag| **keep_flag).count();
        if kept_count <= 2 * II_MAX || pre_epsilon >= epsilon * 0.5 {
            let kept_indices: Vec<usize> = keep
                .iter()
                .enumerate()
                .filter_map(|(i, keep_flag)| keep_flag.then_some(i))
                .collect();
            let prefiltered: Vec<(f64, f64)> = kept_indices.iter().map(|&i| points[i]).collect();
            let rest = epsilon - pre_epsilon;
            return imai_iri_core(&prefiltered, |prefiltered_index| {
                rest * weights[kept_indices[prefiltered_index]]
            });
        }
        pre_epsilon = (pre_epsilon * 2.0).min(epsilon * 0.5);
    }
}

/// An angular interval of directions, ≤ π wide and shrinking under
/// intersection: `u` is inside iff `cross(lo, u) ≥ 0 ∧ cross(u, hi) ≥ 0`.
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
        Wedge {
            lo: (0.0, 0.0),
            hi: (0.0, 0.0),
            any: true,
            empty: false,
        }
    }
    /// Intersects with the wedge of directions whose ray from the anchor
    /// passes within ε of a point at unit direction `center`; `sin_phi` is
    /// ε/dist.
    fn add(&mut self, center: (f64, f64), sin_phi: f64) {
        let cos_phi = (1.0 - sin_phi * sin_phi).max(0.0).sqrt();
        let lo = (
            center.0 * cos_phi + center.1 * sin_phi,
            -center.0 * sin_phi + center.1 * cos_phi,
        );
        let hi = (
            center.0 * cos_phi - center.1 * sin_phi,
            center.0 * sin_phi + center.1 * cos_phi,
        );
        if self.any {
            (self.lo, self.hi, self.any) = (lo, hi, false);
            return;
        }
        // Two arcs each ≤ π wide intersect iff an endpoint of one lies inside
        // the other; when they do, the intersection is [ccw-most start,
        // cw-most end] by MEMBERSHIP, not by pairwise cross comparison — a
        // disjoint interval > 180° away otherwise slips through unchanged.
        let in_current = |u: (f64, f64)| cross(self.lo, u) >= 0.0 && cross(u, self.hi) >= 0.0;
        let in_new = |u: (f64, f64)| cross(lo, u) >= 0.0 && cross(u, hi) >= 0.0;
        let (lo_in, hi_in) = (in_current(lo), in_current(hi));
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
    fn contains(&self, direction: (f64, f64)) -> bool {
        !self.empty
            && (self.any || (cross(self.lo, direction) >= 0.0 && cross(direction, self.hi) >= 0.0))
    }
}

/// Runs the ray-validity sweep from `points[from]`: it walks `ks` (the
/// intermediate points in sweep order), calling `visit(k_target, ok)` where
/// `ok` holds iff the ray from `points[from]` toward `points[k_target]` stays
/// within ε of every point already swept.
/// `dist(p_k, seg(i,j)) ≤ ε_k ⟺ ray-from-i ok ∧ ray-from-j ok`, so two
/// sweeps decide segment validity exactly. The Chan–Chin lemma is pointwise
/// in `k`, so each point may carry its own tolerance `epsilon_of(k)`.
fn ray_sweep(
    points: &[(f64, f64)],
    from: usize,
    ks: impl Iterator<Item = usize>,
    epsilon_of: impl Fn(usize) -> f64 + Copy,
    mut visit: impl FnMut(usize, bool) -> bool,
) {
    let anchor = points[from];
    let mut wedge = Wedge::new();
    let mut has_far = false; // some swept point is > its ε from the anchor
    for k in ks {
        let delta = (points[k].0 - anchor.0, points[k].1 - anchor.1);
        let ok = if delta == (0.0, 0.0) {
            !has_far
        } else {
            wedge.contains(delta)
        };
        if !visit(k, ok) {
            return;
        }
        // fold k into the constraints for the points swept after it
        let dist = (delta.0 * delta.0 + delta.1 * delta.1).sqrt();
        let epsilon_k = epsilon_of(k);
        if dist > epsilon_k {
            has_far = true;
            wedge.add((delta.0 / dist, delta.1 / dist), epsilon_k / dist);
            if wedge.empty {
                // constraints only accumulate — nothing further can be valid
                while visit(usize::MAX, false) {}
                return;
            }
        }
    }
}

fn imai_iri_core(
    points: &[(f64, f64)],
    epsilon_of: impl Fn(usize) -> f64 + Copy,
) -> Vec<(f64, f64)> {
    let n = points.len();
    let words = n.div_ceil(64);
    // lazily computed backward rows: bit i of row j ⟺ ray from p_j toward
    // p_i stays within ε of every point strictly between i and j
    let mut backward_rows: Vec<Option<Vec<u64>>> = vec![None; n];
    let backward_row = |j: usize| -> Vec<u64> {
        let mut bits = vec![0u64; words];
        ray_sweep(points, j, (0..j).rev(), epsilon_of, |i, ok| {
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
        let mut next_frontier = Vec::new();
        for &i in &frontier {
            let mut done = false;
            ray_sweep(points, i, i + 1..n, epsilon_of, |j, forward_ok| {
                if j == usize::MAX {
                    return false; // wedge pinched shut — stop this sweep
                }
                if forward_ok && parent[j] == usize::MAX {
                    let row = backward_rows[j].get_or_insert_with(|| backward_row(j));
                    if row[i / 64] >> (i % 64) & 1 == 1 {
                        parent[j] = i;
                        if j == n - 1 {
                            done = true;
                            return false;
                        }
                        next_frontier.push(j);
                    }
                }
                true
            });
            if done {
                break 'bfs;
            }
        }
        frontier = next_frontier;
    }
    let mut path = vec![n - 1];
    while *path.last().unwrap() != 0 {
        path.push(parent[*path.last().unwrap()]);
    }
    path.iter().rev().map(|&i| points[i]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// deterministic pseudo-random polyline (LCG, same recipe as pip tests)
    fn wiggle(n: usize, seed: u64) -> Vec<(f64, f64)> {
        let mut lcg = utz_common::Lcg::new(seed);
        (0..n)
            .map(
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "i < n ≤ II_MAX+2000 ≪ 2^53; test x-spacing"
                )]
                |i| (i as f64 * 0.1, lcg.unit_f64() * 2.0 - 1.0),
            )
            .collect()
    }

    fn max_deviation(original: &[(f64, f64)], simplified: &[(f64, f64)]) -> f64 {
        // every original point must be near some simplified segment
        original
            .iter()
            .map(|&point| {
                simplified
                    .windows(2)
                    .map(|window| seg_dist2(point, window[0], window[1]))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max)
            .sqrt()
    }

    #[test]
    fn endpoints_always_kept() {
        let points = wiggle(50, 1);
        for out in [
            rdp(&points, 0.5),
            visvalingam(&points, 0.5),
            imai_iri(&points, 0.5),
        ] {
            assert_eq!(out.first(), points.first());
            assert_eq!(out.last(), points.last());
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
    fn epsilon_bound_honored() {
        for seed in 1..=20u64 {
            let points = wiggle(200, seed);
            for epsilon in [0.05, 0.2, 0.8] {
                assert!(
                    max_deviation(&points, &rdp(&points, epsilon)) <= epsilon + 1e-12,
                    "rdp seed {seed}"
                );
                assert!(
                    max_deviation(&points, &imai_iri(&points, epsilon)) <= epsilon + 1e-12,
                    "ii seed {seed}"
                );
            }
        }
    }

    #[test]
    fn imai_iri_never_more_verts_than_rdp() {
        for seed in 1..=20u64 {
            let points = wiggle(200, seed);
            for epsilon in [0.05, 0.2, 0.8] {
                let (rdp_len, imai_iri_len) = (
                    rdp(&points, epsilon).len(),
                    imai_iri(&points, epsilon).len(),
                );
                assert!(
                    imai_iri_len <= rdp_len,
                    "seed {seed} epsilon {epsilon}: imai-iri {imai_iri_len} > rdp {rdp_len}"
                );
            }
        }
    }

    #[test]
    fn imai_iri_optimal_vs_bruteforce() {
        // exhaustively check minimality on small inputs
        fn brute_min(points: &[(f64, f64)], e2: f64) -> usize {
            let n = points.len();
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
                            if rest.first().is_none_or(|&index| index > first) {
                                rest.insert(0, first);
                                out.push(rest);
                            }
                        }
                    }
                    out
                }
                for interior in combos(n, keep - 2) {
                    let mut indices = vec![0];
                    indices.extend(interior);
                    indices.push(n - 1);
                    let ok = indices.windows(2).all(|window| {
                        points[window[0] + 1..window[1]].iter().all(|&point| {
                            seg_dist2(point, points[window[0]], points[window[1]]) <= e2
                        })
                    });
                    if ok {
                        return keep;
                    }
                }
            }
            n
        }
        for seed in 1..=10u64 {
            let points = wiggle(9, seed);
            for epsilon in [0.1, 0.4, 1.0] {
                let imai_iri_len = imai_iri(&points, epsilon).len();
                let optimal_len = brute_min(&points, epsilon * epsilon);
                assert_eq!(imai_iri_len, optimal_len, "seed {seed} epsilon {epsilon}");
            }
        }
    }

    #[test]
    fn visvalingam_monotone_in_threshold() {
        let points = wiggle(200, 7);
        let mut last = usize::MAX;
        for min_area in [0.001, 0.01, 0.1, 1.0] {
            let n = visvalingam(&points, min_area).len();
            assert!(n <= last);
            last = n;
        }
    }

    #[test]
    fn dispatch_matches_direct() {
        let points = wiggle(100, 3);
        assert_eq!(simplify(Simplify::None, &points), points);
        assert_eq!(
            simplify(Simplify::Rdp { epsilon: 0.2 }, &points),
            rdp(&points, 0.2)
        );
        assert_eq!(
            simplify(Simplify::Visvalingam { min_area: 0.2 }, &points),
            visvalingam(&points, 0.2)
        );
        assert_eq!(
            simplify(Simplify::ImaiIri { epsilon: 0.2 }, &points),
            imai_iri(&points, 0.2)
        );
    }

    /// naive O(n) per-check BFS: the reference the wedge core must match
    fn imai_iri_naive(points: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
        let n = points.len();
        let e2 = epsilon * epsilon;
        let valid = |i: usize, j: usize| {
            points[i + 1..j]
                .iter()
                .all(|&point| seg_dist2(point, points[i], points[j]) <= e2)
        };
        let mut parent = vec![usize::MAX; n];
        let mut frontier = vec![0usize];
        parent[0] = 0;
        'bfs: while !frontier.is_empty() {
            let mut next_frontier = Vec::new();
            for &i in &frontier {
                for (j, parent_entry) in parent.iter_mut().enumerate().skip(i + 1) {
                    if *parent_entry == usize::MAX && valid(i, j) {
                        *parent_entry = i;
                        if j == n - 1 {
                            break 'bfs;
                        }
                        next_frontier.push(j);
                    }
                }
            }
            frontier = next_frontier;
        }
        let mut path = vec![n - 1];
        while *path.last().unwrap() != 0 {
            path.push(parent[*path.last().unwrap()]);
        }
        path.iter().rev().map(|&i| points[i]).collect()
    }

    #[test]
    fn wedge_core_matches_naive() {
        // the disjoint-interval wraparound bug needed n ≈ thousands to
        // surface — cover both small and large inputs
        for seed in 1..=30u64 {
            let points = wiggle(400, seed);
            for epsilon in [0.03, 0.1, 0.3, 0.9] {
                let (wedge_len, naive_len) = (
                    imai_iri_core(&points, |_| epsilon).len(),
                    imai_iri_naive(&points, epsilon).len(),
                );
                assert_eq!(
                    wedge_len, naive_len,
                    "seed {seed} epsilon {epsilon}: wedge {wedge_len} != naive {naive_len}"
                );
            }
        }
        for seed in 1..=3u64 {
            let points = wiggle(3000, seed);
            for epsilon in [0.3, 0.9] {
                let (wedge_len, naive_len) = (
                    imai_iri_core(&points, |_| epsilon).len(),
                    imai_iri_naive(&points, epsilon).len(),
                );
                assert_eq!(
                    wedge_len, naive_len,
                    "seed {seed} epsilon {epsilon} (n=3000): wedge {wedge_len} != naive {naive_len}"
                );
            }
        }
    }

    #[test]
    fn long_arc_prefilter_keeps_bound() {
        let points = wiggle(II_MAX + 2000, 11); // > II_MAX → adaptive rdp prefilter + core
        let epsilon = 0.3;
        let out = imai_iri(&points, epsilon);
        assert!(max_deviation(&points, &out) <= epsilon + 1e-12);
        assert!(out.len() < points.len());
    }

    /// distance from one original point to the simplified polyline
    fn point_dev(point: (f64, f64), simplified: &[(f64, f64)]) -> f64 {
        simplified
            .windows(2)
            .map(|window| seg_dist2(point, window[0], window[1]))
            .fold(f64::INFINITY, f64::min)
            .sqrt()
    }

    /// deterministic pseudo-random weights in [lo, 1]
    fn rand_weights(n: usize, lo: f64, seed: u64) -> Vec<f64> {
        let mut lcg = utz_common::Lcg::new(seed);
        (0..n).map(|_| lo + (1.0 - lo) * lcg.unit_f64()).collect()
    }

    #[test]
    fn weighted_uniform_matches_scalar() {
        for seed in 1..=10u64 {
            let points = wiggle(300, seed);
            let ones = vec![1.0; points.len()];
            for epsilon in [0.05, 0.2, 0.8] {
                assert_eq!(
                    rdp_w(&points, epsilon, &ones),
                    rdp(&points, epsilon),
                    "rdp seed {seed}"
                );
                assert_eq!(
                    visvalingam_w(&points, epsilon, &ones),
                    visvalingam(&points, epsilon),
                    "vw seed {seed}"
                );
                assert_eq!(
                    imai_iri_w(&points, epsilon, &ones),
                    imai_iri(&points, epsilon),
                    "ii seed {seed}"
                );
            }
        }
        // prefilter path: the weighted adaptive loop mirrors the scalar one
        let points = wiggle(II_MAX + 2000, 5);
        let ones = vec![1.0; points.len()];
        assert_eq!(imai_iri_w(&points, 0.3, &ones), imai_iri(&points, 0.3));
    }

    #[test]
    fn weighted_per_point_bound() {
        // n ≤ II_MAX so the imai-iri bound is exact (no prefilter composition)
        for seed in 1..=10u64 {
            let points = wiggle(300, seed);
            let weights = rand_weights(points.len(), 0.1, seed);
            let epsilon = 0.4;
            for out in [
                rdp_w(&points, epsilon, &weights),
                imai_iri_w(&points, epsilon, &weights),
            ] {
                for (i, &point) in points.iter().enumerate() {
                    let (deviation, bound) = (point_dev(point, &out), epsilon * weights[i]);
                    assert!(
                        deviation <= bound + 1e-12,
                        "seed {seed} pt {i}: {deviation} > {bound}"
                    );
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
        let mut weights = vec![1.0; line.len()];
        weights[5] = 0.1;
        assert!(!rdp(&line, 0.5).contains(&bump));
        assert!(rdp_w(&line, 0.5, &weights).contains(&bump));
        assert!(!visvalingam(&line, 0.5).contains(&bump));
        assert!(visvalingam_w(&line, 0.5, &weights).contains(&bump));
        // every imai-iri shortcut spanning the bump misses it by 0.3 > 0.05,
        // so the bump itself must be a shortcut endpoint
        assert!(!imai_iri(&line, 0.5).contains(&bump));
        assert!(imai_iri_w(&line, 0.5, &weights).contains(&bump));
    }

    #[test]
    fn weighted_long_arc_prefilter_bound() {
        // > II_MAX: per-point bound may relax where weights vary within a
        // prefilter shortcut, but the global epsilon·max(weights) bound always
        // holds
        let points = wiggle(II_MAX + 2000, 13);
        #[expect(
            clippy::cast_precision_loss,
            reason = "i < points.len() = II_MAX+2000 ≪ 2^53; test weights"
        )]
        let weights: Vec<f64> = (0..points.len())
            .map(|i| 0.6 + 0.4 * (i as f64 / 500.0).sin())
            .collect();
        let epsilon = 0.3;
        let out = imai_iri_w(&points, epsilon, &weights);
        assert!(out.len() < points.len());
        for &point in &points {
            assert!(point_dev(point, &out) <= epsilon + 1e-12);
        }
    }

    #[test]
    fn weighted_dispatch_matches_direct() {
        let points = wiggle(100, 3);
        let weights = rand_weights(points.len(), 0.2, 9);
        assert_eq!(simplify_weighted(Simplify::None, &points, &weights), points);
        assert_eq!(
            simplify_weighted(Simplify::Rdp { epsilon: 0.2 }, &points, &weights),
            rdp_w(&points, 0.2, &weights)
        );
        assert_eq!(
            simplify_weighted(Simplify::Visvalingam { min_area: 0.2 }, &points, &weights),
            visvalingam_w(&points, 0.2, &weights)
        );
        assert_eq!(
            simplify_weighted(Simplify::ImaiIri { epsilon: 0.2 }, &points, &weights),
            imai_iri_w(&points, 0.2, &weights)
        );
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "weight() knees are exact early-returns (1.0/w_min); approximate equality would weaken the test"
    )]
    fn density_weight_shape() {
        let model = DensityWeight::new(0.1);
        assert_eq!(model.weight(0.0), 1.0);
        assert_eq!(model.weight(model.d_lo), 1.0);
        assert_eq!(model.weight(1e12), 0.1);
        assert_eq!(model.weight(f64::NAN), 1.0);
        // continuous at both knees, monotone nonincreasing between
        assert!((model.weight(model.d_lo * 1.0001) - 1.0).abs() < 1e-3);
        assert!((model.weight(model.d_hi * 0.9999) - 0.1).abs() < 1e-3);
        let mut last = 1.0;
        for i in 0..100 {
            let density = model.d_lo * (model.d_hi / model.d_lo).powf(f64::from(i) / 99.0);
            let weight = model.weight(density);
            assert!(
                weight <= last + 1e-15 && (model.w_min..=1.0).contains(&weight),
                "density={density} weight={weight}"
            );
            last = weight;
        }
        // weighting off
        assert_eq!(DensityWeight::new(1.0).weight(1e9), 1.0);
    }
}
