//! Format B: TopoJSON-style topology. Shared borders are cut into *arcs* at
//! junctions, each arc stored ONCE as i24 delta+varint, and every ring is a list
//! of signed arc references. Optional topology-aware Ramer–Douglas–Peucker
//! (RDP) line simplification runs on each arc a single time (endpoints fixed),
//! so neighbouring polygons stay stitched. Other open-polyline simplifiers
//! could slot into the same per-arc pass (PLAN.md §14).

use std::collections::HashMap;

use crate::Feat;
// simplification lives in utz-simplify (shared with the viz HTML via WASM)
pub use utz_simplify::{simplify, Simplify};

// quantization parameterized by bit-width (i16 abs, i24 abs, i32 abs, ...)
fn qmax_of(bits: u32) -> f64 { ((1u64 << (bits - 1)) - 1) as f64 }
fn qxb(lon: f64, qmax: f64) -> i32 { (lon / 180.0 * qmax).round() as i32 }
fn qyb(lat: f64, qmax: f64) -> i32 { (lat / 90.0 * qmax).round() as i32 }
fn pushb(out: &mut Vec<u8>, v: i32, bits: u32) {
    let n = ((bits + 7) / 8) as usize; // bytes per axis (i16->2, i24->3, i32->4)
    out.extend_from_slice(&v.to_le_bytes()[0..n]);
}

fn zigzag(v: i64) -> u64 { ((v << 1) ^ (v >> 63)) as u64 }
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop { let b = (v & 0x7f) as u8; v >>= 7; if v == 0 { out.push(b); break; } else { out.push(b | 0x80); } }
}

type VId = u32;

pub struct TopoOut {
    pub bytes: Vec<u8>,
    pub arcs: usize,
    pub ring_refs: usize,
    pub verts: usize,          // vertices actually stored (after simplification)
    pub simplified: Vec<Feat>, // geometry reconstructed from the (simplified) arcs
}

/// The shared-arc topology itself, before any serialization: what the container
/// encoder (encode.rs) consumes. Arc coords are f64, already RDP-simplified.
pub struct Topology {
    pub arc_coords: Vec<Vec<(f64, f64)>>,
    /// per ring: signed arc refs (`id << 1 | reversed`)
    pub ring_refs: Vec<Vec<u32>>,
    /// feature → polygon → ring indices into `ring_refs`
    pub structure: Vec<Vec<Vec<usize>>>,
}

impl Topology {
    /// Reconstruct feature geometry from (possibly re-quantized) arc coords.
    pub fn reconstruct(&self, feats: &[Feat], arc_coords: &[Vec<(f64, f64)>]) -> Vec<Feat> {
        let ring_coords = |ring_idx: usize| -> Vec<(f64, f64)> {
            let mut c: Vec<(f64, f64)> = Vec::new();
            for &r in &self.ring_refs[ring_idx] {
                let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
                let mut a = arc_coords[id].clone();
                if rev { a.reverse(); }
                if c.last() == a.first() { a.remove(0); }
                c.extend(a);
            }
            if c.last() == c.first() && c.len() > 1 { c.pop(); }
            c
        };
        feats.iter().enumerate().map(|(fi, f)| {
            let polys = self.structure[fi].iter()
                .map(|poly| poly.iter().map(|&ri| ring_coords(ri)).collect())
                .collect();
            Feat { offset: f.offset, tzid: f.tzid.clone(), polys }
        }).collect()
    }
}

pub fn encode_topology(feats: &[Feat], eps_deg: f64) -> TopoOut {
    encode_topology_q(feats, eps_deg, 24)
}

/// `qbits` selects the absolute grid: 16 = i16 (~611 m lon), 24 = i24 (~2.4 m), 32 = cm.
pub fn encode_topology_q(feats: &[Feat], eps_deg: f64, qbits: u32) -> TopoOut {
    encode_topology_qm(feats, eps_deg, qbits, false)
}

/// Steps 1–4 of Format B: dedup vertices, cut shared arcs at junctions,
/// topology-aware RDP (each arc simplified exactly once, endpoints fixed).
pub fn build_topology(feats: &[Feat], eps_deg: f64) -> Topology {
    build_topology_algo(feats, Simplify::Rdp { eps: eps_deg })
}

/// [`build_topology`] with the simplification algorithm as a knob
/// (`utz-simplify` menu: RDP / Visvalingam–Whyatt / Imai–Iri / None).
pub fn build_topology_algo(feats: &[Feat], algo: Simplify) -> Topology {
    build_topology_impl(feats, algo, None)
}

/// [`build_topology_algo`] with spatially varying tolerance: `edge_weight(a, b)`
/// returns the tolerance multiplier for one arc edge (in practice
/// `DensityWeight::weight(DensityGrid::max_along(a, b))`), and each vertex
/// simplifies under the *smallest* multiplier of its incident edges — so a
/// long edge crossing a dense area pins both flanking vertices. Weights are a
/// pure function of arc geometry and every shared arc is simplified exactly
/// once, so neighbouring zones stay stitched by construction.
pub fn build_topology_weighted(feats: &[Feat], algo: Simplify, edge_weight: &dyn Fn((f64, f64), (f64, f64)) -> f64) -> Topology {
    build_topology_impl(feats, algo, Some(edge_weight))
}

fn build_topology_impl(feats: &[Feat], algo: Simplify, edge_weight: Option<&dyn Fn((f64, f64), (f64, f64)) -> f64>) -> Topology {
    // 1. dedup vertices (bit-exact) -> ids + coords
    let mut vid: HashMap<(u64, u64), VId> = HashMap::new();
    let mut vcoord: Vec<(f64, f64)> = Vec::new();
    let get = |x: f64, y: f64, vid: &mut HashMap<(u64, u64), VId>, vc: &mut Vec<(f64, f64)>| -> VId {
        *vid.entry((x.to_bits(), y.to_bits())).or_insert_with(|| { vc.push((x, y)); (vc.len() - 1) as VId })
    };
    let mut rings: Vec<Vec<VId>> = Vec::new();
    let mut structure: Vec<Vec<Vec<usize>>> = Vec::new();
    for f in feats {
        let mut fpolys = Vec::new();
        for p in &f.polys {
            let mut pr = Vec::new();
            for r in p {
                let seq: Vec<VId> = r.iter().map(|&(x, y)| get(x, y, &mut vid, &mut vcoord)).collect();
                pr.push(rings.len()); rings.push(seq);
            }
            fpolys.push(pr);
        }
        structure.push(fpolys);
    }

    // 2. owner signature per undirected edge
    let mut owners: HashMap<(VId, VId), Vec<u32>> = HashMap::new();
    for (ri, seq) in rings.iter().enumerate() {
        let n = seq.len();
        for i in 0..n {
            let (a, b) = (seq[i], seq[(i + 1) % n]);
            let key = if a < b { (a, b) } else { (b, a) };
            let e = owners.entry(key).or_default();
            if e.last() != Some(&(ri as u32)) { e.push(ri as u32); }
        }
    }
    let mut sig_ids: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut edge_sig: HashMap<(VId, VId), u32> = HashMap::new();
    for (k, v) in &owners {
        let mut s = v.clone(); s.sort_unstable(); s.dedup();
        let next = sig_ids.len() as u32;
        let id = *sig_ids.entry(s).or_insert(next);
        edge_sig.insert(*k, id);
    }
    let sig = |a: VId, b: VId| -> u32 { edge_sig[&if a < b { (a, b) } else { (b, a) }] };

    // 3. cut rings into arcs at junctions; dedup arcs (seq or reverse)
    let mut arc_ids: HashMap<Vec<VId>, u32> = HashMap::new();
    let mut arcs: Vec<Vec<VId>> = Vec::new();
    let mut ring_refs: Vec<Vec<u32>> = vec![Vec::new(); rings.len()];
    let intern = |seq: Vec<VId>, ai: &mut HashMap<Vec<VId>, u32>, av: &mut Vec<Vec<VId>>| -> u32 {
        let mut rev = seq.clone(); rev.reverse();
        let (canon, dir) = if seq <= rev { (seq, 0u32) } else { (rev, 1u32) };
        let next = av.len() as u32;
        let id = *ai.entry(canon.clone()).or_insert_with(|| { av.push(canon); next });
        (id << 1) | dir
    };
    for (ri, seq) in rings.iter().enumerate() {
        let n = seq.len();
        if n == 0 { continue; }
        let mut cuts: Vec<usize> = Vec::new();
        for i in 0..n {
            if sig(seq[(i + n - 1) % n], seq[i]) != sig(seq[i], seq[(i + 1) % n]) { cuts.push(i); }
        }
        if cuts.is_empty() {
            // junction-free closed ring (an island / lone hole): the same
            // ring shared by two features (island outline = hole of the zone
            // around it) must intern to ONE arc regardless of where each
            // feature's ring starts or which way it winds. Canonical form =
            // lexicographically smallest closed walk over BOTH directions
            // from every occurrence of the smallest vertex id (a pinched
            // ring can pass through it twice, and the per-direction lexmins
            // can differ — picking only the forward one would make the two
            // windings disagree). intern() gets the ring's own-winding walk
            // so its direction bit still preserves ring orientation.
            let m = *seq.iter().min().unwrap();
            let mut best: Option<(Vec<VId>, bool)> = None; // (closed walk, forward here?)
            for i in (0..n).filter(|&i| seq[i] == m) {
                let fwd: Vec<VId> = (0..=n).map(|k| seq[(i + k) % n]).collect();
                let bwd: Vec<VId> = (0..=n).map(|k| seq[(i + n - k) % n]).collect();
                for (cand, f) in [(fwd, true), (bwd, false)] {
                    if best.as_ref().map_or(true, |(b, _)| cand < *b) { best = Some((cand, f)); }
                }
            }
            let (canon, forward) = best.unwrap();
            let a = if forward { canon } else { let mut r = canon; r.reverse(); r };
            ring_refs[ri].push(intern(a, &mut arc_ids, &mut arcs));
        } else {
            for j in 0..cuts.len() {
                let (start, end) = (cuts[j], cuts[(j + 1) % cuts.len()]);
                let mut a = Vec::new(); let mut k = start;
                loop { a.push(seq[k]); if k == end { break; } k = (k + 1) % n; }
                ring_refs[ri].push(intern(a, &mut arc_ids, &mut arcs));
            }
        }
    }

    // 4. arc coords (+ topology-aware simplification, each arc once)
    let arc_coords: Vec<Vec<(f64, f64)>> = arcs.iter()
        .map(|a| {
            let c: Vec<(f64, f64)> = a.iter().map(|&v| vcoord[v as usize]).collect();
            match edge_weight {
                None => simplify(algo, &c),
                Some(f) => {
                    let ew: Vec<f64> = c.windows(2).map(|p| f(p[0], p[1])).collect();
                    let w: Vec<f64> = (0..c.len())
                        .map(|i| {
                            let left = if i > 0 { ew[i - 1] } else { f64::INFINITY };
                            let right = ew.get(i).copied().unwrap_or(f64::INFINITY);
                            left.min(right).min(1.0) // refine-only, endpoints kept anyway
                        })
                        .collect();
                    utz_simplify::simplify_weighted(algo, &c, &w)
                }
            }
        })
        .collect();

    Topology { arc_coords, ring_refs, structure }
}

/// `abs_fixed`: store arc vertices as fixed-width absolute ints (random-access)
/// instead of the default delta + zigzag-varint stream.
pub fn encode_topology_qm(feats: &[Feat], eps_deg: f64, qbits: u32, abs_fixed: bool) -> TopoOut {
    let qmax = qmax_of(qbits);
    let topo = build_topology(feats, eps_deg);
    let Topology { arc_coords, ring_refs, structure } = &topo;
    let verts: usize = arc_coords.iter().map(|a| a.len()).sum();
    let simplified = topo.reconstruct(feats, arc_coords);

    // 5. serialize
    let total_refs: usize = ring_refs.iter().map(|r| r.len()).sum();
    let mut pool: Vec<String> = Vec::new();
    let mut sidx: HashMap<String, u16> = HashMap::new();
    for f in feats {
        if let Some(t) = &f.tzid { if !sidx.contains_key(t) { sidx.insert(t.clone(), pool.len() as u16); pool.push(t.clone()); } }
    }
    let mut o = Vec::new();
    o.extend_from_slice(&0x4E45_4442u32.to_le_bytes());
    o.extend_from_slice(&(feats.len() as u32).to_le_bytes());
    o.extend_from_slice(&(pool.len() as u16).to_le_bytes());
    for s in &pool { o.extend_from_slice(&(s.len() as u16).to_le_bytes()); o.extend_from_slice(s.as_bytes()); }
    o.extend_from_slice(&(arc_coords.len() as u32).to_le_bytes());
    for a in arc_coords {
        put_varint(&mut o, a.len() as u64);
        let (mut px, mut py) = (0i64, 0i64);
        for (i, &(x, y)) in a.iter().enumerate() {
            let (cx, cy) = (qxb(x, qmax) as i64, qyb(y, qmax) as i64);
            if abs_fixed {
                pushb(&mut o, cx as i32, qbits); pushb(&mut o, cy as i32, qbits);
            } else if i == 0 {
                pushb(&mut o, cx as i32, qbits); pushb(&mut o, cy as i32, qbits);
            } else {
                put_varint(&mut o, zigzag(cx - px)); put_varint(&mut o, zigzag(cy - py));
            }
            px = cx; py = cy;
        }
    }
    for (fi, f) in feats.iter().enumerate() {
        o.extend_from_slice(&(f.offset as f32).to_le_bytes());
        let ti = f.tzid.as_ref().map(|t| sidx[t]).unwrap_or(0xFFFF);
        o.extend_from_slice(&ti.to_le_bytes());
        let (mut nx, mut ny, mut xx, mut xy) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for p in &f.polys { for r in p { for &(x, y) in r { let (a, b) = (qxb(x, qmax), qyb(y, qmax)); nx = nx.min(a); ny = ny.min(b); xx = xx.max(a); xy = xy.max(b); }}}
        for v in [nx, ny, xx, xy] { pushb(&mut o, v, qbits); }
        o.extend_from_slice(&(structure[fi].len() as u16).to_le_bytes());
        for poly in &structure[fi] {
            o.extend_from_slice(&(poly.len() as u16).to_le_bytes());
            for &ring_idx in poly {
                put_varint(&mut o, ring_refs[ring_idx].len() as u64);
                for &r in &ring_refs[ring_idx] { put_varint(&mut o, r as u64); }
            }
        }
    }
    TopoOut { bytes: o, arcs: arc_coords.len(), ring_refs: total_refs, verts, simplified }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Poly, Ring};

    /// Two unit squares sharing the x=1 border, whose shared edge carries
    /// interior vertices with 0.001 bumps (fodder for simplification).
    fn two_squares() -> Vec<Feat> {
        let shared: Vec<(f64, f64)> = (0..=10)
            .map(|i| (1.0 + if i % 2 == 1 { 0.001 } else { 0.0 }, i as f64 / 10.0))
            .collect(); // (1,0) … (1,1), odd indices bumped east
        let mut left: Ring = vec![(0.0, 0.0)];
        left.extend(&shared); // up the shared border
        left.push((0.0, 1.0));
        let mut right: Ring = vec![(2.0, 0.0), (2.0, 1.0)];
        right.extend(shared.iter().rev()); // down the shared border
        let f = |r: Ring| Feat { offset: 0.0, tzid: None, polys: vec![vec![r] as Poly] };
        vec![f(left), f(right)]
    }

    /// An island whose outline is also the hole of the zone around it — the
    /// same closed ring, but each feature starts it at a different vertex and
    /// winds it the opposite way (exactly the Cyprus case). The topology must
    /// intern that ring as ONE arc, not two rotated copies.
    #[test]
    fn shared_island_ring_interns_once() {
        let island: Ring = vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0)];
        // same cycle, rotated to a different start and reversed
        let hole: Ring = vec![(2.0, 2.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0)];
        let sea: Ring = vec![(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (0.0, 3.0)];
        let feats = vec![
            Feat { offset: 0.0, tzid: None, polys: vec![vec![island] as Poly] },
            Feat { offset: 1.0, tzid: None, polys: vec![vec![sea, hole] as Poly] },
        ];
        let t = build_topology_algo(&feats, Simplify::Rdp { eps: 0.0 });
        // sea outline + island ring shared once = 2 arcs, not 3
        assert_eq!(t.arc_coords.len(), 2, "island ring duplicated: {:?}", t.arc_coords);
        // reconstruction must still round-trip both features' ring vertex sets
        let rec = t.reconstruct(&feats, &t.arc_coords);
        for (f, r) in rec.iter().zip(&feats) {
            for (p, q) in f.polys.iter().zip(&r.polys) {
                for (ring, orig) in p.iter().zip(q) {
                    let mut a: Vec<_> = ring.iter().map(|&(x, y)| (x.to_bits(), y.to_bits())).collect();
                    let mut b: Vec<_> = orig.iter().map(|&(x, y)| (x.to_bits(), y.to_bits())).collect();
                    a.sort_unstable(); b.sort_unstable();
                    assert_eq!(a, b);
                }
            }
        }
    }

    /// A pinched (figure-eight) ring passes through its smallest vertex
    /// twice, so the lexicographically smallest FORWARD rotation differs
    /// between the two windings — canonicalization must consider both walk
    /// directions or the two features intern two different arcs.
    #[test]
    fn shared_pinched_ring_interns_once() {
        let eight: Ring = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0), (-1.0, 0.0), (-1.0, -1.0)];
        // same cycle, reversed and rotated to a different start
        let eight_rev: Ring = vec![(1.0, 1.0), (1.0, 0.0), (0.0, 0.0), (-1.0, -1.0), (-1.0, 0.0), (0.0, 0.0)];
        let feats = vec![
            Feat { offset: 0.0, tzid: None, polys: vec![vec![eight] as Poly] },
            Feat { offset: 1.0, tzid: None, polys: vec![vec![eight_rev] as Poly] },
        ];
        let t = build_topology_algo(&feats, Simplify::Rdp { eps: 0.0 });
        assert_eq!(t.arc_coords.len(), 1, "pinched ring duplicated: {:?}", t.arc_coords);
        // both rings still round-trip their vertex multiset
        let rec = t.reconstruct(&feats, &t.arc_coords);
        for (f, r) in rec.iter().zip(&feats) {
            let mut a: Vec<_> = f.polys[0][0].iter().map(|&(x, y)| (x.to_bits(), y.to_bits())).collect();
            let mut b: Vec<_> = r.polys[0][0].iter().map(|&(x, y)| (x.to_bits(), y.to_bits())).collect();
            a.sort_unstable(); b.sort_unstable();
            assert_eq!(a, b);
        }
    }

    #[test]
    fn weighted_all_ones_matches_unweighted() {
        let feats = two_squares();
        let t0 = build_topology_algo(&feats, Simplify::Rdp { eps: 0.01 });
        let t1 = build_topology_weighted(&feats, Simplify::Rdp { eps: 0.01 }, &|_, _| 1.0);
        assert_eq!(t0.arc_coords, t1.arc_coords);
        assert_eq!(t0.ring_refs, t1.ring_refs);
    }

    #[test]
    fn weighted_shared_arc_consistent() {
        let feats = two_squares();
        let (kept_bump, dropped_bump) = ((1.001, 0.5), (1.001, 0.1));
        // "dense" stretch around y=0.5: edges whose midpoint falls in it get a
        // small multiplier (0.05 * 0.01 = 0.0005 < the 0.001 bumps)
        let weight = |a: (f64, f64), b: (f64, f64)| {
            if (0.42..=0.58).contains(&((a.1 + b.1) / 2.0)) { 0.05 } else { 1.0 }
        };
        let t = build_topology_weighted(&feats, Simplify::Rdp { eps: 0.01 }, &weight);
        let rec = t.reconstruct(&feats, &t.arc_coords);
        for f in &rec {
            let ring = &f.polys[0][0];
            // the weighted stretch survives in BOTH zones (arc shared once)…
            assert!(ring.contains(&kept_bump), "{:?}", ring);
            // …and the uniform-weight stretches still simplify away
            assert!(!ring.contains(&dropped_bump), "{:?}", ring);
        }
        // unweighted at the same eps drops every bump
        let rec0 = {
            let t0 = build_topology_algo(&feats, Simplify::Rdp { eps: 0.01 });
            t0.reconstruct(&feats, &t0.arc_coords)
        };
        assert!(!rec0[0].polys[0][0].contains(&kept_bump));
    }
}
