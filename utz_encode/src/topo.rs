//! The TopoJSON-style shared-arc topology builder. Shared borders are cut
//! into *arcs* at junctions, each arc is stored ONCE as i24 delta+varint,
//! and every ring is a list
//! of signed arc references. Optional topology-aware Ramer–Douglas–Peucker
//! (RDP) line simplification runs on each arc a single time (endpoints fixed),
//! so neighbouring polygons stay stitched. Other open-polyline simplifiers
//! could slot into the same per-arc pass.

use std::collections::HashMap;

use crate::{Arc, Feat};
// simplification lives in utz_simplify (shared with the viz HTML via WASM)
pub use utz_simplify::{simplify, Simplify};

// quantization parameterized by bit-width (i16 abs, i24 abs, i32 abs, ...)
use crate::{q_lat, q_lon, qmax_for};
fn pushb(out: &mut Vec<u8>, value: i32, bits: u32) {
    let byte_len = bits.div_ceil(8) as usize; // bytes per axis (i16->2, i24->3, i32->4)
    out.extend_from_slice(&value.to_le_bytes()[0..byte_len]);
}

/// Zigzag-encodes a signed delta for [`put_varint()`]; the reader's
/// inverse is `utz::format::unzigzag()`.
#[must_use]
pub fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)).cast_unsigned()
}
/// Appends `value` as a varint, low 7 bits per byte with the high bit
/// as continuation; the reader's inverse is `utz::format::read_varint()`.
pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

type VId = u32;

/// A serialized topology plus the counts and reconstructed geometry the
/// measurement tools report on.
pub struct TopoOut {
    /// The serialized arc store (i24 delta+varint).
    pub bytes: Vec<u8>,
    /// The number of shared arcs stored.
    pub arcs: usize,
    /// The number of ring-to-arc references across all rings.
    pub ring_refs: usize,
    /// The number of vertices actually stored (after simplification).
    pub verts: usize,
    /// The geometry reconstructed from the (simplified) arcs.
    pub simplified: Vec<Feat>,
}

/// The shared-arc topology itself, before any serialization: this is what
/// the container encoder (encode.rs) consumes. Arc coords are f64 and
/// already RDP-simplified.
pub struct Topology {
    pub arc_coords: Vec<Arc>,
    /// The signed arc refs of each ring (`id << 1 | reversed`).
    pub ring_refs: Vec<Vec<u32>>,
    /// The feature → polygon → ring nesting; the innermost values index
    /// into `ring_refs`.
    pub structure: Vec<Vec<Vec<usize>>>,
}

impl Topology {
    /// Reconstructs feature geometry from (possibly re-quantized) arc
    /// coords.
    #[must_use]
    pub fn reconstruct(&self, feats: &[Feat], arc_coords: &[Vec<(f64, f64)>]) -> Vec<Feat> {
        let ring_coords = |ring_index: usize| -> Vec<(f64, f64)> {
            let mut coords: Vec<(f64, f64)> = Vec::new();
            for &arc_ref in &self.ring_refs[ring_index] {
                let (id, reversed) = ((arc_ref >> 1) as usize, (arc_ref & 1) == 1);
                let mut arc = arc_coords[id].clone();
                if reversed {
                    arc.reverse();
                }
                if coords.last() == arc.first() {
                    arc.remove(0);
                }
                coords.extend(arc);
            }
            if coords.last() == coords.first() && coords.len() > 1 {
                coords.pop();
            }
            coords
        };
        feats
            .iter()
            .enumerate()
            .map(|(feature_index, feature)| {
                let polys = self.structure[feature_index]
                    .iter()
                    .map(|poly| {
                        poly.iter()
                            .map(|&ring_index| ring_coords(ring_index))
                            .collect()
                    })
                    .collect();
                Feat {
                    offset: feature.offset,
                    tzid: feature.tzid.clone(),
                    polys,
                }
            })
            .collect()
    }
}

/// Builds and serializes the shared-arc topology at the default i24
/// quantization: [`build_topology()`] runs first, followed by arc-store
/// serialization.
#[must_use]
pub fn encode_topology(feats: &[Feat], epsilon_deg: f64) -> TopoOut {
    encode_topology_q(feats, epsilon_deg, 24)
}

/// `qbits` selects the absolute grid: 16 gives i16 (~611 m lon), 24 gives
/// i24 (~2.4 m), and 32 gives cm precision.
#[must_use]
pub fn encode_topology_q(feats: &[Feat], epsilon_deg: f64, qbits: u32) -> TopoOut {
    encode_topology_qm(feats, epsilon_deg, qbits, false)
}

/// Builds the topology: vertices are deduped, shared arcs are cut at
/// junctions, and topology-aware RDP runs on each arc exactly once with
/// endpoints fixed.
#[must_use]
pub fn build_topology(feats: &[Feat], epsilon_deg: f64) -> Topology {
    build_topology_algorithm(
        feats,
        Simplify::Rdp {
            epsilon: epsilon_deg,
        },
    )
}

/// This is [`build_topology()`] with the simplification algorithm as a knob
/// (the `utz_simplify` menu: RDP, Visvalingam–Whyatt, Imai–Iri, or None).
#[must_use]
pub fn build_topology_algorithm(feats: &[Feat], algorithm: Simplify) -> Topology {
    build_topology_impl(feats, algorithm, None)
}

/// This is [`build_topology_algorithm()`] with spatially varying tolerance:
/// `edge_weight(a, b)`
/// returns the tolerance multiplier for one arc edge (in practice
/// `DensityWeight::weight(DensityGrid::max_along(a, b))`), and each vertex
/// simplifies under the *smallest* multiplier of its incident edges, so a
/// long edge crossing a dense area pins both flanking vertices. Weights are a
/// pure function of arc geometry and every shared arc is simplified exactly
/// once, so neighbouring zones stay stitched by construction.
pub fn build_topology_weighted(
    feats: &[Feat],
    algorithm: Simplify,
    edge_weight: &EdgeWeightFn<'_>,
) -> Topology {
    build_topology_impl(feats, algorithm, Some(edge_weight))
}

/// The tolerance multiplier for the edge `a`–`b` (see
/// [`build_topology_weighted()`]).
pub type EdgeWeightFn<'a> = dyn Fn((f64, f64), (f64, f64)) -> f64 + 'a;

#[expect(
    clippy::too_many_lines,
    reason = "one pass over shared vertex/arc tables; stage extraction would thread six mutable maps through every helper"
)]
fn build_topology_impl(
    feats: &[Feat],
    algorithm: Simplify,
    edge_weight: Option<&EdgeWeightFn<'_>>,
) -> Topology {
    // 1. dedup vertices (bit-exact) -> ids + coords
    let mut vertex_ids: HashMap<(u64, u64), VId> = HashMap::new();
    let mut vertex_coords: Vec<(f64, f64)> = Vec::new();
    let intern_vertex = |x: f64,
                         y: f64,
                         vertex_ids: &mut HashMap<(u64, u64), VId>,
                         vertex_coords: &mut Vec<(f64, f64)>|
     -> VId {
        *vertex_ids
            .entry((x.to_bits(), y.to_bits()))
            .or_insert_with(|| {
                vertex_coords.push((x, y));
                VId::try_from(vertex_coords.len() - 1).expect("vertex id fits u32")
            })
    };
    let mut rings: Vec<Vec<VId>> = Vec::new();
    let mut structure: Vec<Vec<Vec<usize>>> = Vec::new();
    for feature in feats {
        let mut feature_polys = Vec::new();
        for poly in &feature.polys {
            let mut poly_rings = Vec::new();
            for ring in poly {
                let vertex_seq: Vec<VId> = ring
                    .iter()
                    .map(|&(x, y)| intern_vertex(x, y, &mut vertex_ids, &mut vertex_coords))
                    .collect();
                poly_rings.push(rings.len());
                rings.push(vertex_seq);
            }
            feature_polys.push(poly_rings);
        }
        structure.push(feature_polys);
    }

    // 2. owner signature per undirected edge
    let mut owners: HashMap<(VId, VId), Vec<u32>> = HashMap::new();
    for (ring_index, vertex_seq) in rings.iter().enumerate() {
        let len = vertex_seq.len();
        for i in 0..len {
            let (a, b) = (vertex_seq[i], vertex_seq[(i + 1) % len]);
            let key = if a < b { (a, b) } else { (b, a) };
            let entry = owners.entry(key).or_default();
            let ring_index_u32 = u32::try_from(ring_index).expect("ring id fits u32");
            if entry.last() != Some(&ring_index_u32) {
                entry.push(ring_index_u32);
            }
        }
    }
    let mut signature_ids: HashMap<Vec<u32>, u32> = HashMap::new();
    let mut edge_signature: HashMap<(VId, VId), u32> = HashMap::new();
    for (edge, owner_rings) in &owners {
        let mut sorted_owners = owner_rings.clone();
        sorted_owners.sort_unstable();
        sorted_owners.dedup();
        let next = u32::try_from(signature_ids.len()).expect("arc id fits u32");
        let id = *signature_ids.entry(sorted_owners).or_insert(next);
        edge_signature.insert(*edge, id);
    }
    let signature =
        |a: VId, b: VId| -> u32 { edge_signature[&if a < b { (a, b) } else { (b, a) }] };

    // 3. cut rings into arcs at junctions; dedup arcs (sequence or reverse)
    let mut arc_ids: HashMap<Vec<VId>, u32> = HashMap::new();
    let mut arcs: Vec<Vec<VId>> = Vec::new();
    let mut ring_refs: Vec<Vec<u32>> = vec![Vec::new(); rings.len()];
    let intern = |vertex_seq: Vec<VId>,
                  arc_ids: &mut HashMap<Vec<VId>, u32>,
                  arcs: &mut Vec<Vec<VId>>|
     -> u32 {
        let mut reversed = vertex_seq.clone();
        reversed.reverse();
        let (canonical, direction) = if vertex_seq <= reversed {
            (vertex_seq, 0u32)
        } else {
            (reversed, 1u32)
        };
        let next = u32::try_from(arcs.len()).expect("arc id fits u32");
        let id = *arc_ids.entry(canonical.clone()).or_insert_with(|| {
            arcs.push(canonical);
            next
        });
        (id << 1) | direction
    };
    for (ring_index, vertex_seq) in rings.iter().enumerate() {
        let len = vertex_seq.len();
        if len == 0 {
            continue;
        }
        let mut cuts: Vec<usize> = Vec::new();
        for i in 0..len {
            if signature(vertex_seq[(i + len - 1) % len], vertex_seq[i])
                != signature(vertex_seq[i], vertex_seq[(i + 1) % len])
            {
                cuts.push(i);
            }
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
            let min_vertex = *vertex_seq.iter().min().unwrap();
            let mut best: Option<(Vec<VId>, bool)> = None; // (closed walk, forward here?)
            for i in (0..len).filter(|&i| vertex_seq[i] == min_vertex) {
                let forward_walk: Vec<VId> = (0..=len).map(|k| vertex_seq[(i + k) % len]).collect();
                let backward_walk: Vec<VId> =
                    (0..=len).map(|k| vertex_seq[(i + len - k) % len]).collect();
                for (candidate, is_forward) in [(forward_walk, true), (backward_walk, false)] {
                    if best
                        .as_ref()
                        .is_none_or(|(best_walk, _)| candidate < *best_walk)
                    {
                        best = Some((candidate, is_forward));
                    }
                }
            }
            let (canonical, forward) = best.unwrap();
            let walk = if forward {
                canonical
            } else {
                let mut reversed = canonical;
                reversed.reverse();
                reversed
            };
            ring_refs[ring_index].push(intern(walk, &mut arc_ids, &mut arcs));
        } else {
            for j in 0..cuts.len() {
                let (start, end) = (cuts[j], cuts[(j + 1) % cuts.len()]);
                let mut arc_seq = Vec::new();
                let mut k = start;
                loop {
                    arc_seq.push(vertex_seq[k]);
                    if k == end {
                        break;
                    }
                    k = (k + 1) % len;
                }
                ring_refs[ring_index].push(intern(arc_seq, &mut arc_ids, &mut arcs));
            }
        }
    }

    // 4. arc coords (+ topology-aware simplification, each arc once)
    let arc_coords: Vec<Arc> = arcs
        .iter()
        .map(|arc| {
            let coords: Vec<(f64, f64)> = arc
                .iter()
                .map(|&vertex_id| vertex_coords[vertex_id as usize])
                .collect();
            match edge_weight {
                None => simplify(algorithm, &coords),
                Some(weight_fn) => {
                    let edge_weights: Vec<f64> = coords
                        .windows(2)
                        .map(|pair| weight_fn(pair[0], pair[1]))
                        .collect();
                    let vertex_weights: Vec<f64> = (0..coords.len())
                        .map(|i| {
                            let left = if i > 0 {
                                edge_weights[i - 1]
                            } else {
                                f64::INFINITY
                            };
                            let right = edge_weights.get(i).copied().unwrap_or(f64::INFINITY);
                            left.min(right).min(1.0) // refine-only, endpoints kept anyway
                        })
                        .collect();
                    utz_simplify::simplify_weighted(algorithm, &coords, &vertex_weights)
                }
            }
        })
        .collect();

    Topology {
        arc_coords,
        ring_refs,
        structure,
    }
}

/// `abs_fixed` stores arc vertices as fixed-width absolute ints
/// (random-access) instead of the default delta + zigzag-varint stream.
#[must_use]
///
/// # Panics
/// Panics if a count or quantized coordinate exceeds its serialized width
/// (u16 pool/poly counts, u32 arc ids, i32 coords).
#[expect(
    clippy::too_many_lines,
    reason = "linear serialization of one container; the stages share the running buffer and section offsets"
)]
pub fn encode_topology_qm(
    feats: &[Feat],
    epsilon_deg: f64,
    qbits: u32,
    abs_fixed: bool,
) -> TopoOut {
    let qmax = qmax_for(qbits);
    let topo = build_topology(feats, epsilon_deg);
    let Topology {
        arc_coords,
        ring_refs,
        structure,
    } = &topo;
    let verts: usize = arc_coords.iter().map(std::vec::Vec::len).sum();
    let simplified = topo.reconstruct(feats, arc_coords);

    // 5. serialize
    let total_refs: usize = ring_refs.iter().map(std::vec::Vec::len).sum();
    let mut pool: Vec<String> = Vec::new();
    let mut tzid_indices: HashMap<String, u16> = HashMap::new();
    for feature in feats {
        if let Some(tzid) = &feature.tzid {
            if !tzid_indices.contains_key(tzid) {
                tzid_indices.insert(
                    tzid.clone(),
                    u16::try_from(pool.len()).expect("tzid pool index fits u16"),
                );
                pool.push(tzid.clone());
            }
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x4E45_4442u32.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(feats.len())
            .expect("feature count fits u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(
        &u16::try_from(pool.len())
            .expect("tzid pool count fits u16")
            .to_le_bytes(),
    );
    for tzid in &pool {
        out.extend_from_slice(
            &u16::try_from(tzid.len())
                .expect("tzid length fits u16")
                .to_le_bytes(),
        );
        out.extend_from_slice(tzid.as_bytes());
    }
    out.extend_from_slice(
        &u32::try_from(arc_coords.len())
            .expect("arc count fits u32")
            .to_le_bytes(),
    );
    for arc in arc_coords {
        put_varint(&mut out, arc.len() as u64);
        let (mut previous_x, mut previous_y) = (0i64, 0i64);
        for (i, &(lon, lat)) in arc.iter().enumerate() {
            let (current_x, current_y) = (i64::from(q_lon(lon, qmax)), i64::from(q_lat(lat, qmax)));
            if abs_fixed || i == 0 {
                pushb(
                    &mut out,
                    i32::try_from(current_x).expect("quantized coord fits i32"),
                    qbits,
                );
                pushb(
                    &mut out,
                    i32::try_from(current_y).expect("quantized coord fits i32"),
                    qbits,
                );
            } else {
                put_varint(&mut out, zigzag(current_x - previous_x));
                put_varint(&mut out, zigzag(current_y - previous_y));
            }
            previous_x = current_x;
            previous_y = current_y;
        }
    }
    for (feature_index, feature) in feats.iter().enumerate() {
        #[expect(clippy::cast_possible_truncation, reason = "f32 header field")]
        let offset32 = feature.offset as f32;
        out.extend_from_slice(&offset32.to_le_bytes());
        let tzid_index = feature
            .tzid
            .as_ref()
            .map_or(0xFFFF, |tzid| tzid_indices[tzid]);
        out.extend_from_slice(&tzid_index.to_le_bytes());
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for poly in &feature.polys {
            for ring in poly {
                for &(lon, lat) in ring {
                    let (x, y) = (q_lon(lon, qmax), q_lat(lat, qmax));
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        for value in [min_x, min_y, max_x, max_y] {
            pushb(&mut out, value, qbits);
        }
        out.extend_from_slice(
            &u16::try_from(structure[feature_index].len())
                .expect("poly count fits u16")
                .to_le_bytes(),
        );
        for poly in &structure[feature_index] {
            out.extend_from_slice(
                &u16::try_from(poly.len())
                    .expect("ring count fits u16")
                    .to_le_bytes(),
            );
            for &ring_index in poly {
                put_varint(&mut out, ring_refs[ring_index].len() as u64);
                for &arc_ref in &ring_refs[ring_index] {
                    put_varint(&mut out, u64::from(arc_ref));
                }
            }
        }
    }
    TopoOut {
        bytes: out,
        arcs: arc_coords.len(),
        ring_refs: total_refs,
        verts,
        simplified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Poly, Ring};

    /// Two unit squares sharing the x=1 border, whose shared edge carries
    /// interior vertices with 0.001 bumps (fodder for simplification).
    fn two_squares() -> Vec<Feat> {
        let shared: Vec<(f64, f64)> = (0..=10)
            .map(|i| {
                (
                    1.0 + if i % 2 == 1 { 0.001 } else { 0.0 },
                    f64::from(i) / 10.0,
                )
            })
            .collect(); // (1,0) … (1,1), odd indices bumped east
        let mut left: Ring = vec![(0.0, 0.0)];
        left.extend(&shared); // up the shared border
        left.push((0.0, 1.0));
        let mut right: Ring = vec![(2.0, 0.0), (2.0, 1.0)];
        right.extend(shared.iter().rev()); // down the shared border
        let make_feature = |ring: Ring| Feat {
            offset: 0.0,
            tzid: None,
            polys: vec![vec![ring] as Poly],
        };
        vec![make_feature(left), make_feature(right)]
    }

    /// An island whose outline is also the hole of the zone around it: the
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
            Feat {
                offset: 0.0,
                tzid: None,
                polys: vec![vec![island] as Poly],
            },
            Feat {
                offset: 1.0,
                tzid: None,
                polys: vec![vec![sea, hole] as Poly],
            },
        ];
        let topology = build_topology_algorithm(&feats, Simplify::Rdp { epsilon: 0.0 });
        // sea outline + island ring shared once = 2 arcs, not 3
        assert_eq!(
            topology.arc_coords.len(),
            2,
            "island ring duplicated: {:?}",
            topology.arc_coords
        );
        // reconstruction must still round-trip both features' ring vertex sets
        let reconstructed = topology.reconstruct(&feats, &topology.arc_coords);
        for (feature, original) in reconstructed.iter().zip(&feats) {
            for (polys, original_polys) in feature.polys.iter().zip(&original.polys) {
                for (ring, original_ring) in polys.iter().zip(original_polys) {
                    let mut actual: Vec<_> = ring
                        .iter()
                        .map(|&(x, y)| (x.to_bits(), y.to_bits()))
                        .collect();
                    let mut expected: Vec<_> = original_ring
                        .iter()
                        .map(|&(x, y)| (x.to_bits(), y.to_bits()))
                        .collect();
                    actual.sort_unstable();
                    expected.sort_unstable();
                    assert_eq!(actual, expected);
                }
            }
        }
    }

    /// A pinched (figure-eight) ring passes through its smallest vertex
    /// twice, so the lexicographically smallest FORWARD rotation differs
    /// between the two windings. Canonicalization must consider both walk
    /// directions or the two features intern two different arcs.
    #[test]
    fn shared_pinched_ring_interns_once() {
        let eight: Ring = vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (1.0, 1.0),
            (0.0, 0.0),
            (-1.0, 0.0),
            (-1.0, -1.0),
        ];
        // same cycle, reversed and rotated to a different start
        let eight_rev: Ring = vec![
            (1.0, 1.0),
            (1.0, 0.0),
            (0.0, 0.0),
            (-1.0, -1.0),
            (-1.0, 0.0),
            (0.0, 0.0),
        ];
        let feats = vec![
            Feat {
                offset: 0.0,
                tzid: None,
                polys: vec![vec![eight] as Poly],
            },
            Feat {
                offset: 1.0,
                tzid: None,
                polys: vec![vec![eight_rev] as Poly],
            },
        ];
        let topology = build_topology_algorithm(&feats, Simplify::Rdp { epsilon: 0.0 });
        assert_eq!(
            topology.arc_coords.len(),
            1,
            "pinched ring duplicated: {:?}",
            topology.arc_coords
        );
        // both rings still round-trip their vertex multiset
        let reconstructed = topology.reconstruct(&feats, &topology.arc_coords);
        for (feature, original) in reconstructed.iter().zip(&feats) {
            let mut actual: Vec<_> = feature.polys[0][0]
                .iter()
                .map(|&(x, y)| (x.to_bits(), y.to_bits()))
                .collect();
            let mut expected: Vec<_> = original.polys[0][0]
                .iter()
                .map(|&(x, y)| (x.to_bits(), y.to_bits()))
                .collect();
            actual.sort_unstable();
            expected.sort_unstable();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn weighted_all_ones_matches_unweighted() {
        let feats = two_squares();
        let unweighted = build_topology_algorithm(&feats, Simplify::Rdp { epsilon: 0.01 });
        let weighted =
            build_topology_weighted(&feats, Simplify::Rdp { epsilon: 0.01 }, &|_, _| 1.0);
        assert_eq!(unweighted.arc_coords, weighted.arc_coords);
        assert_eq!(unweighted.ring_refs, weighted.ring_refs);
    }

    #[test]
    fn weighted_shared_arc_consistent() {
        let feats = two_squares();
        let (kept_bump, dropped_bump) = ((1.001, 0.5), (1.001, 0.1));
        // "dense" stretch around y=0.5: edges whose midpoint falls in it get a
        // small multiplier (0.05 * 0.01 = 0.0005 < the 0.001 bumps)
        let weight = |a: (f64, f64), b: (f64, f64)| {
            if (0.42..=0.58).contains(&f64::midpoint(a.1, b.1)) {
                0.05
            } else {
                1.0
            }
        };
        let topology = build_topology_weighted(&feats, Simplify::Rdp { epsilon: 0.01 }, &weight);
        let reconstructed = topology.reconstruct(&feats, &topology.arc_coords);
        for feature in &reconstructed {
            let ring = &feature.polys[0][0];
            // the weighted stretch survives in BOTH zones (arc shared once)…
            assert!(ring.contains(&kept_bump), "{ring:?}");
            // …and the uniform-weight stretches still simplify away
            assert!(!ring.contains(&dropped_bump), "{ring:?}");
        }
        // unweighted at the same epsilon drops every bump
        let reconstructed_unweighted = {
            let unweighted = build_topology_algorithm(&feats, Simplify::Rdp { epsilon: 0.01 });
            unweighted.reconstruct(&feats, &unweighted.arc_coords)
        };
        assert!(!reconstructed_unweighted[0].polys[0][0].contains(&kept_bump));
    }
}
