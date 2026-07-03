//! Format B: TopoJSON-style topology. Shared borders are cut into *arcs* at
//! junctions, each arc stored ONCE as i24 delta+varint, and every ring is a list
//! of signed arc references. Optional topology-aware RDP simplifies each arc a
//! single time (endpoints fixed), so neighbouring polygons stay stitched.

use std::collections::HashMap;

use crate::{Feat, Poly};

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

/// open-polyline RDP keeping both endpoints; result has >= 2 points.
fn rdp_open(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 || eps <= 0.0 { return pts.to_vec(); }
    let mut keep = vec![false; pts.len()];
    keep[0] = true; *keep.last_mut().unwrap() = true;
    rec(pts, 0, pts.len() - 1, eps * eps, &mut keep);
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(&p, _)| p).collect()
}
fn rec(p: &[(f64, f64)], a: usize, b: usize, e2: f64, keep: &mut [bool]) {
    if b <= a + 1 { return; }
    let (ax, ay) = p[a]; let (bx, by) = p[b];
    let (dx, dy) = (bx - ax, by - ay); let len2 = dx * dx + dy * dy;
    let (mut im, mut dm) = (a, 0.0);
    for i in a + 1..b {
        let (px, py) = p[i];
        let d2 = if len2 == 0.0 { (px - ax).powi(2) + (py - ay).powi(2) }
            else { let t = ((px - ax) * dx + (py - ay) * dy) / len2; let (cx, cy) = (ax + t * dx, ay + t * dy); (px - cx).powi(2) + (py - cy).powi(2) };
        if d2 > dm { dm = d2; im = i; }
    }
    if dm > e2 { keep[im] = true; rec(p, a, im, e2, keep); rec(p, im, b, e2, keep); }
}

pub fn encode_topology(feats: &[Feat], eps_deg: f64) -> TopoOut {
    encode_topology_q(feats, eps_deg, 24)
}

/// `qbits` selects the absolute grid: 16 = i16 (~611 m lon), 24 = i24 (~2.4 m), 32 = cm.
pub fn encode_topology_q(feats: &[Feat], eps_deg: f64, qbits: u32) -> TopoOut {
    encode_topology_qm(feats, eps_deg, qbits, false)
}

/// `abs_fixed`: store arc vertices as fixed-width absolute ints (random-access)
/// instead of the default delta + zigzag-varint stream.
pub fn encode_topology_qm(feats: &[Feat], eps_deg: f64, qbits: u32, abs_fixed: bool) -> TopoOut {
    let qmax = qmax_of(qbits);
    // 1. dedup vertices (bit-exact) -> ids + coords
    let mut vid: HashMap<(u64, u64), VId> = HashMap::new();
    let mut vcoord: Vec<(f64, f64)> = Vec::new();
    let mut get = |x: f64, y: f64, vid: &mut HashMap<(u64, u64), VId>, vc: &mut Vec<(f64, f64)>| -> VId {
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
    let mut intern = |seq: Vec<VId>, ai: &mut HashMap<Vec<VId>, u32>, av: &mut Vec<Vec<VId>>| -> u32 {
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
            let mut a = seq.clone(); a.push(seq[0]);
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

    // 4. arc coords (+ topology-aware RDP, each arc once)
    let arc_coords: Vec<Vec<(f64, f64)>> = arcs.iter()
        .map(|a| { let c: Vec<(f64, f64)> = a.iter().map(|&v| vcoord[v as usize]).collect(); rdp_open(&c, eps_deg) })
        .collect();
    let verts: usize = arc_coords.iter().map(|a| a.len()).sum();

    // reconstruct simplified geometry (for accuracy testing)
    let decode = |r: u32| -> (usize, bool) { ((r >> 1) as usize, (r & 1) == 1) };
    let ring_coords = |ring_idx: usize| -> Vec<(f64, f64)> {
        let mut c: Vec<(f64, f64)> = Vec::new();
        for &r in &ring_refs[ring_idx] {
            let (id, rev) = decode(r);
            let mut a = arc_coords[id].clone();
            if rev { a.reverse(); }
            if c.last() == a.first() { a.remove(0); }
            c.extend(a);
        }
        if c.last() == c.first() { c.pop(); }
        c
    };
    let mut simplified: Vec<Feat> = Vec::new();
    for (fi, f) in feats.iter().enumerate() {
        let mut polys: Vec<Poly> = Vec::new();
        for poly in &structure[fi] {
            let mut rc: Poly = Vec::new();
            for &ring_idx in poly { rc.push(ring_coords(ring_idx)); }
            polys.push(rc);
        }
        simplified.push(Feat { offset: f.offset, tzid: f.tzid.clone(), polys });
    }

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
    for a in &arc_coords {
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
