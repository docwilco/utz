//! Self-describing container serializer (PLAN.md §4/§5 step 7).
//!
//! Layout (all little-endian):
//! ```text
//! outer:  magic "uTZ1" | version u8 | codec u8 | raw_len u32 | payload…
//!         (raw_len = UNCOMPRESSED payload size, so decoders allocate once)
//! payload (compressed per codec):
//!   header:     dataset u8 | quant_bits u8 | simplify_algo u8
//!               | grid_deg f32 | eps_m f32
//!               | tzbb_release (len u8 + bytes)
//!               | n_features u16 | arcs_off u32 | rings_off u32 | grid_off u32
//!               | eager_coords u32 | eager_rings u32 | eager_polys u32
//!               (eager-cache sizes so `preload` reserves exactly — no growth
//!               doubling; coords is Σ referenced-arc vcounts, a ≤0.1% over-
//!               estimate of the deduped cache, safe as a reservation)
//!   zone table: str_offsets u16[n_features+1] | tzid pool bytes   (zone i = feature i)
//!   arc store:  n_arcs u32 | arc_offsets u32[n_arcs+1] (relative to arc data)
//!               | per arc: varint vcount | first vertex i{16,24,32}×2
//!               | zigzag-varint deltas
//!   ring index (v4): parent u16[n_polys] + poly_offsets u32[n_polys+1]
//!   (relative to ring data) — grid candidates are polys
//!               | per feature: npolys u16; per poly: bbox i{16,24,32}×4
//!               | nrings u16; per ring: varint nrefs | varint signed arc refs
//!               (id<<1|rev)
//!   grid:       ncols u16 | nrows u16 | primary u16[ncols*nrows]
//!               | uniq u16 | list_offsets u16[uniq+1] | list_ids u16[Σ]
//! ```
//! The header records every knob, so the runtime decoder stays generic (§4).
//! Grid + bboxes are derived from the QUANTIZED geometry, so what the runtime
//! PIPs is exactly what the grid indexed.


use crate::error::ensure;
use crate::grid::{self, Order};
use crate::{clean, topo, Error, Feat};

// on-disk magic stays ASCII ("μ" is 2 bytes in UTF-8 and byte literals
// reject non-ASCII); the project brands as μTZ, the container as uTZ1
pub const MAGIC: [u8; 4] = *b"uTZ1";
pub const VERSION: u8 = 7; // v7: flags byte + image coords at quant width
                           // (i24 packed, optional ring alignment); v6 12-byte
                           // outer + EagerImage; v5 bbox; v4 poly grid; v3 geom
/// Outer container header length (v6): magic4 + version + codec + `raw_len` u32
/// + 2 reserved/pad bytes so a 4-aligned container gives a 4-aligned payload.
pub const OUTER_LEN: usize = 12;

/// Checked narrowing for serializer counts/offsets: the format stores these
/// at fixed width and a wrap would silently corrupt the container, so panic.
/// Data-dependent limits (feature count, tzid pool, CSR tables) are
/// `ensure!`-guarded with [`Error::FormatLimit`] before these run.
fn c32(n: usize) -> u32 { u32::try_from(n).expect("exceeds u32 format width") }
fn c16(n: usize) -> u16 { u16::try_from(n).expect("exceeds u16 format width") }

#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
pub enum Codec {
    Uncompressed = 0,
    Gzip = 1,
    Zstd = 2,
    Brotli = 3,
    Xz = 4,
}

/// Simplification algorithm recorded in the header (§14.8) and applied by
/// [`build_payload`]. RDP is the default; Imai–Iri gives provably minimum
/// vertices for the same ε bound (slower encode). Visvalingam has an area
/// knob, not ε — reachable via `topo::build_topology_algo`, not this byte's
/// eps-driven pipeline.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(u8)]
pub enum SimplifyAlgo {
    #[default]
    Rdp = 0,
    Visvalingam = 1,
    ImaiIri = 2,
}

/// Arc-store encoding, recorded in the header (PLAN §13/§15 fixed-width
/// measurements). `DeltaVarint` is the flash-size default. `Fixed` stores
/// absolute fixed-width coords: raw arcs +40–72%, best-compressed +24–32%
/// (xz overtakes brotli) — bought: streaming lookups skip the per-vertex
/// varint decode, the dominant cost on embedded (near-eager speed, zero
/// RAM), so it suits XIP `-static` assets.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
#[repr(u8)]
pub enum GeomEncoding {
    #[default]
    DeltaVarint = 0,
    Fixed = 1,
    /// The geometry section IS the preload cache (§15): flattened per-ring
    /// `(i32, i32)` runs + ring/poly index tables, 4-aligned — the slice
    /// kernels run straight off flash (`from_static`: eager speed, zero RAM,
    /// zero boot) or straight off the decompressed buffer (`from_slice`:
    /// no preload pass). No arc store; shared arcs duplicated per ring:
    /// raw ~4.1–4.3× the varint payload, best-compressed +61–94% (xz).
    EagerImage = 2,
    /// Grid-only asset (§10/§15): header + tzid pool + parent + grid, no
    /// geometry at all. `lookup()` answers at cell precision (== the
    /// dominant-first coarse answer) — precision is an asset property, like
    /// `eps_m`. Smallest flash by far (~⅓ of even the varint payload for
    /// tiny); endianness-independent; the `geom-coarse` reader compiles no
    /// PIP code.
    Coarse = 3,
}

impl SimplifyAlgo {
    /// ε-driven `Simplify` for the topology builder.
    ///
    /// # Errors
    ///
    /// `Visvalingam` is rejected: its knob is an area, not ε (see the message
    /// for the workaround).
    pub fn to_simplify(self, eps_deg: f64) -> crate::Result<utz_simplify::Simplify> {
        Ok(match self {
            SimplifyAlgo::Rdp => utz_simplify::Simplify::Rdp { eps: eps_deg },
            SimplifyAlgo::ImaiIri => utz_simplify::Simplify::ImaiIri { eps: eps_deg },
            SimplifyAlgo::Visvalingam => return Err(Error::VisvalingamEps),
        })
    }
}

pub struct Params<'a> {
    /// bits 0–1: vintage (0 = now, 1 = 1970, 2 = all/comprehensive);
    /// bit 2 set = land-only (clear = with-oceans). See `utz_build::Dataset::code`.
    pub dataset: u8,
    /// TZBB release tag recorded in the header (DST vintage / cache key)
    pub tzbb_release: &'a str,
    pub eps_m: f64,
    /// 16 / 24 / 32
    pub quant_bits: u32,
    /// grid cell size in degrees, 0.1–45 — fractional (0.5, 4/3, …) allowed
    pub grid_deg: f64,
    pub codec: Codec,
    /// simplification algorithm (§14.8): applied by [`build_payload`],
    /// recorded in the header either way
    pub simplify: SimplifyAlgo,
    /// arc-store encoding: delta+varint (default) or fixed-width
    pub geom: GeomEncoding,
}

/// Byte size of each payload section + post-simplification geometry counts —
/// the viewer's "delta+varint" stage stats.
#[derive(Clone, Copy, Default, Debug)]
pub struct PayloadStats {
    pub header: u32,
    pub zones: u32,
    pub arcs: u32,
    pub rings: u32,
    pub grid: u32,
    pub n_arcs: u32,
    /// vertices actually stored (post-simplify, post-quantize-clean)
    pub n_verts: u32,
    /// what the post-quantization cleanup removed (see clean.rs)
    pub clean: clean::CleanStats,
}

/// Full uniform-ε pipeline: topology → RDP → quantize → grid → serialize →
/// compress. Spatially varying tolerance (population weighting) is a
/// simplification concern, not a serialization one: build the topology
/// yourself (`topo::build_topology_weighted`) and use
/// [`payload_from_topology`] + [`finish`] — see utz-build's wrapper.
///
/// # Errors
///
/// Same as [`build_payload`].
pub fn encode(feats: &[Feat], p: &Params) -> crate::Result<Vec<u8>> {
    finish(&build_payload(feats, p)?, p.codec)
}

/// Everything but the outer header + compression (so size sweeps can compress
/// one payload with several codecs).
///
/// # Errors
///
/// `p.simplify == Visvalingam` (see [`SimplifyAlgo::to_simplify`]); otherwise
/// as [`payload_from_topology`].
pub fn build_payload(feats: &[Feat], p: &Params) -> crate::Result<Vec<u8>> {
    let algo = p.simplify.to_simplify(p.eps_m / 111_320.0)?;
    let t = topo::build_topology_algo(feats, algo);
    Ok(payload_from_topology(&t, &t.arc_coords, feats, p)?.0)
}

/// Serialize an already-simplified topology: quantize → grid → sections.
/// `arc_coords` may differ from `t.arc_coords` (the wasm viewer simplifies
/// per-arc itself); `feats` supplies only per-feature metadata (tzid, offset)
/// — geometry comes from the arcs. `p.eps_m` is recorded in the header, not
/// applied.
///
/// # Errors
///
/// Out-of-range params (`quant_bits` not 16/24/32, `grid_deg` outside 0.1–45,
/// `tzbb_release` ≥ 256 bytes) or format-limit overflows (feature/polygon
/// counts past 15-bit zone ids, CSR list tables past their 15-bit/u16 space,
/// eager coordinate count past u32).
///
/// # Panics
/// If a serialized section outgrows its u16/u32 format width where no
/// [`Error::FormatLimit`] guard applies (payload over 4 GiB — unreachable
/// for real datasets).
pub fn payload_from_topology(
    t: &topo::Topology,
    arc_coords: &[Vec<(f64, f64)>],
    feats: &[Feat],
    p: &Params,
) -> crate::Result<(Vec<u8>, PayloadStats)> {
    ensure!(matches!(p.quant_bits, 16 | 24 | 32), Error::QuantBits { bits: p.quant_bits });
    ensure!((0.1..=45.0).contains(&p.grid_deg), Error::GridDeg { deg: p.grid_deg });
    ensure!(
        feats.len() < 0x7FFF,
        Error::FormatLimit { what: "feature count (15-bit zone ids)", n: feats.len(), max: 0x7FFE }
    );
    let qmax = ((1u64 << (p.quant_bits - 1)) - 1) as f64;
    #[expect(clippy::cast_possible_truncation, reason = "lon bounded, |lon/180*qmax| < i32::MAX; float as saturates")]
    let qx = |lon: f64| (lon / 180.0 * qmax).round() as i32;
    #[expect(clippy::cast_possible_truncation, reason = "lat bounded, |lat/90*qmax| < i32::MAX; float as saturates")]
    let qy = |lat: f64| (lat / 90.0 * qmax).round() as i32;
    let dq = |v: i32, half: f64| f64::from(v) / qmax * half;

    // quantize arcs, then clean the snapping artifacts per shared arc (dups,
    // zero-area spikes, collinear pass-throughs) and drop rings that
    // collapsed to zero area — see clean.rs. Junction endpoints stay put, so
    // neighbouring zones remain stitched.
    let mut cst = clean::CleanStats::default();
    let arcs_q: Vec<Vec<(i32, i32)>> = arc_coords.iter().map(|a| {
        let mut q: Vec<(i32, i32)> = a.iter().map(|&(x, y)| (qx(x), qy(y))).collect();
        let closed = a.len() > 1 && a.first() == a.last();
        clean::clean_arc(&mut q, closed, &mut cst);
        q
    }).collect();
    let (ring_refs, structure, arcs_q) =
        clean::drop_degenerate_rings(&t.ring_refs, &t.structure, arcs_q, &mut cst);
    let t = topo::Topology { arc_coords: Vec::new(), ring_refs, structure };

    // grid over the dequantized geometry = exactly what the runtime sees.
    // v4: rasterized per POLYGON, not per feature — border-cell candidate
    // lists carry poly ids, so lookups jump straight to the ~2 polys whose
    // rings touch the cell instead of bbox-scanning every poly of every
    // candidate feature (§10/§15 polygrid_probe: 20-24 polys parsed → 2.1,
    // per-poly bboxes redundant, CSR growth ≈ dropped bbox bytes).
    let arcs_dq: Vec<Vec<(f64, f64)>> = arcs_q.iter()
        .map(|a| a.iter().map(|&(x, y)| (dq(x, 180.0), dq(y, 90.0))).collect())
        .collect();
    let quantized = t.reconstruct(feats, &arcs_dq);
    let mut poly_feats: Vec<Feat> = Vec::new();
    let mut parent: Vec<u16> = Vec::new(); // poly id -> feature id
    for (fi, qf) in quantized.iter().enumerate() {
        for poly in &qf.polys {
            poly_feats.push(Feat { offset: 0.0, tzid: None, polys: vec![poly.clone()] });
            parent.push(c16(fi));
        }
    }
    ensure!(
        poly_feats.len() < 0x7FFF,
        Error::FormatLimit { what: "polygon count (15-bit ids)", n: poly_feats.len(), max: 0x7FFE }
    );
    let g = grid::build(&poly_feats, p.grid_deg, 8);
    let areas = grid::feat_areas(&poly_feats);
    let mut csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);
    // interior/single cells answer without PIP — store the FEATURE id
    // (coarse answers need no parent hop); border lists keep poly ids
    for v in &mut csr.primary {
        if *v & 0x8000 == 0 && *v != 0x7FFF {
            *v = parent[*v as usize];
        }
    }
    // the format's CSR tables are u16 (§4): border-cell tags carry a 15-bit
    // list index, and list_offsets index into list_ids as u16 — a fine grid
    // on a dense dataset can overflow both. Fail loudly instead of the `as
    // u16` wrap silently corrupting the tables.
    ensure!(
        csr.uniq_lists < 0x7FFF,
        Error::GridLists { deg: p.grid_deg, n: csr.uniq_lists }
    );
    ensure!(
        u16::try_from(csr.list_ids.len()).is_ok(),
        Error::GridListIds { deg: p.grid_deg, n: csr.list_ids.len() }
    );

    // EagerImage (geom=2): flatten the geometry into the exact preload-cache
    // shape now, so the header's eager counts are exact and the sections
    // below just serialize it. Junction dedup stays within a ring.
    let image = if p.geom == GeomEncoding::EagerImage {
        let mut coords: Vec<(i32, i32)> = Vec::new();
        let mut ring_ends: Vec<u32> = Vec::new();
        let mut ipolys: Vec<([i32; 4], u32)> = Vec::new();
        for fi in 0..feats.len() {
            for poly in &t.structure[fi] {
                let mut bb = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
                for &ri in poly {
                    let rstart = coords.len();
                    for &r in &t.ring_refs[ri] {
                        let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
                        let seg = coords.len();
                        let a = &arcs_q[id];
                        if rev {
                            coords.extend(a.iter().rev());
                        } else {
                            coords.extend(a.iter());
                        }
                        // drop the duplicated junction vertex between arcs
                        if seg > rstart && coords.get(seg - 1) == coords.get(seg) {
                            coords.remove(seg);
                        }
                    }
                    // drop the duplicated ring-closure vertex (ring_hit wraps)
                    if coords.len() > rstart + 1 && coords.last() == coords.get(rstart) {
                        coords.pop();
                    }
                    for &(x, y) in &coords[rstart..] {
                        bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
                    }
                    ring_ends.push(c32(coords.len()));
                }
                ipolys.push((bb, c32(ring_ends.len())));
            }
        }
        Some((coords, ring_ends, ipolys))
    } else {
        None
    };

    let mut stats = PayloadStats {
        n_arcs: c32(arcs_q.len()),
        n_verts: arcs_q.iter().map(|a| c32(a.len())).sum(),
        clean: cst,
        ..Default::default()
    };
    // ---- header ----
    let mut o = vec![
        p.dataset,
        u8::try_from(p.quant_bits).expect("quant_bits guarded to 16/24/32"),
        p.simplify as u8,
        p.geom as u8,
        0, // flags: reserved, zero
    ];
    #[expect(clippy::cast_possible_truncation, reason = "f32 header field")]
    let grid_deg32 = p.grid_deg as f32;
    o.extend_from_slice(&grid_deg32.to_le_bytes());
    #[expect(clippy::cast_possible_truncation, reason = "f32 header field")]
    let eps_m32 = p.eps_m as f32;
    o.extend_from_slice(&eps_m32.to_le_bytes());
    ensure!(
        p.tzbb_release.len() < 256,
        Error::FormatLimit { what: "tzbb_release bytes", n: p.tzbb_release.len(), max: 255 }
    );
    o.push(u8::try_from(p.tzbb_release.len()).expect("guarded < 256"));
    o.extend_from_slice(p.tzbb_release.as_bytes());
    o.extend_from_slice(&c16(feats.len()).to_le_bytes());
    let fixup = o.len(); // arcs_off, rings_off, grid_off patched below
    o.extend_from_slice(&[0u8; 12]);
    // eager-cache reservation counts (v2): what preload will hold, known
    // exactly here — coords as Σ referenced-arc vcounts (junction dedup at
    // decode shrinks it a hair; a reservation may only over-estimate).
    // EagerImage headers carry the EXACT image counts (they locate the
    // sections, not a reservation).
    let (eager_coords, eager_rings, eager_polys) = match &image {
        Some((coords, ring_ends, ipolys)) => {
            (coords.len() as u64, c32(ring_ends.len()), c32(ipolys.len()))
        }
        // coarse: no geometry — polys counts the parent table entries
        None if p.geom == GeomEncoding::Coarse => (0, 0, c32(parent.len())),
        None => {
            let mut coords: u64 = 0;
            let (mut rings, mut polys) = (0u32, 0u32);
            for fi in 0..feats.len() {
                for poly in &t.structure[fi] {
                    polys += 1;
                    for &ri in poly {
                        rings += 1;
                        coords += t.ring_refs[ri]
                            .iter()
                            .map(|&r| arcs_q[(r >> 1) as usize].len() as u64)
                            .sum::<u64>();
                    }
                }
            }
            (coords, rings, polys)
        }
    };
    ensure!(
        u32::try_from(eager_coords).is_ok(),
        Error::FormatLimit { what: "eager_coords", n: usize::try_from(eager_coords).unwrap_or(usize::MAX), max: u32::MAX as usize }
    );
    ensure!(
        eager_polys as usize == parent.len(),
        Error::FormatLimit { what: "eager_polys (must equal parent count)", n: eager_polys as usize, max: parent.len() }
    );
    o.extend_from_slice(&u32::try_from(eager_coords).expect("guarded above").to_le_bytes());
    o.extend_from_slice(&eager_rings.to_le_bytes());
    o.extend_from_slice(&eager_polys.to_le_bytes());
    stats.header = c32(o.len());

    // ---- zone table (zone i = feature i) ----
    // the offsets are u16 on disk — fail loudly instead of the `as u16`
    // wrap silently corrupting the table (same policy as the CSR guards)
    let total_pool: usize = feats.iter().map(|f| f.tzid.as_deref().unwrap_or("").len()).sum();
    ensure!(
        u16::try_from(total_pool).is_ok(),
        Error::FormatLimit { what: "tzid pool bytes (u16 offsets)", n: total_pool, max: u16::MAX as usize }
    );
    let mut str_off: Vec<u16> = Vec::with_capacity(feats.len() + 1);
    let mut pool: Vec<u8> = Vec::new();
    for f in feats {
        str_off.push(c16(pool.len()));
        pool.extend_from_slice(f.tzid.as_deref().unwrap_or("").as_bytes());
    }
    str_off.push(c16(pool.len()));
    for v in &str_off { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&pool);

    let push_fixed = |o: &mut Vec<u8>, v: i32| {
        let n = (p.quant_bits as usize).div_ceil(8);
        o.extend_from_slice(&v.to_le_bytes()[0..n]);
    };
    let (arcs_off, rings_off);
    if p.geom == GeomEncoding::Coarse {
        // ---- coarse (geom=3): no geometry sections, just the parent table
        // (border-cell candidate poly ids still resolve to features) ----
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        rings_off = c32(o.len());
        for &pf in &parent { o.extend_from_slice(&pf.to_le_bytes()); }
    } else if let Some((coords, ring_ends, ipolys)) = &image {
        // ---- eager-image geometry (geom=2): [coords (i32,i32)][ring_ends
        // u32][polys bbox 4×i32 + rend u32], coords 4-aligned within the
        // payload (the 12-byte outer header keeps it 4-aligned in flash) ----
        while o.len() % 4 != 0 {
            o.push(0);
        }
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        // coords at quant width (v7): i16 4 B/vertex, i24 packed 6 B, i32 8 B
        for &(x, y) in coords {
            push_fixed(&mut o, x);
            push_fixed(&mut o, y);
        }
        for v in ring_ends {
            o.extend_from_slice(&v.to_le_bytes());
        }
        for &(bb, rend) in ipolys {
            for v in bb {
                o.extend_from_slice(&v.to_le_bytes());
            }
            o.extend_from_slice(&rend.to_le_bytes());
        }
        // ---- ring index reduces to the parent table ----
        rings_off = c32(o.len());
        stats.arcs = rings_off - arcs_off;
        for &pf in &parent { o.extend_from_slice(&pf.to_le_bytes()); }
    } else {
        // ---- arc store ----
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        o.extend_from_slice(&c32(arcs_q.len()).to_le_bytes());
        let mut arc_data = Vec::new();
        let mut arc_offsets: Vec<u32> = Vec::with_capacity(arcs_q.len() + 1);
        for a in &arcs_q {
            arc_offsets.push(c32(arc_data.len()));
            put_varint(&mut arc_data, a.len() as u64);
            match p.geom {
                GeomEncoding::DeltaVarint => {
                    let (mut px, mut py) = (0i64, 0i64);
                    for (i, &(x, y)) in a.iter().enumerate() {
                        if i == 0 {
                            push_fixed(&mut arc_data, x);
                            push_fixed(&mut arc_data, y);
                        } else {
                            put_varint(&mut arc_data, zigzag(i64::from(x) - px));
                            put_varint(&mut arc_data, zigzag(i64::from(y) - py));
                        }
                        (px, py) = (i64::from(x), i64::from(y));
                    }
                }
                GeomEncoding::Fixed => {
                    for &(x, y) in a {
                        push_fixed(&mut arc_data, x);
                        push_fixed(&mut arc_data, y);
                    }
                }
                GeomEncoding::EagerImage | GeomEncoding::Coarse => {
                    unreachable!("handled above")
                }
            }
        }
        arc_offsets.push(c32(arc_data.len()));
        for v in &arc_offsets { o.extend_from_slice(&v.to_le_bytes()); }
        o.extend_from_slice(&arc_data);

        // ---- ring index (v4: per-poly records — grid candidates are polys;
        // parent table maps them to features) ----
        rings_off = c32(o.len());
        stats.arcs = rings_off - arcs_off;
        for &pf in &parent { o.extend_from_slice(&pf.to_le_bytes()); }
        let mut ring_data = Vec::new();
        let mut poly_offsets: Vec<u32> = Vec::with_capacity(parent.len() + 1);
        for fi in 0..feats.len() {
            for poly in &t.structure[fi] {
                poly_offsets.push(c32(ring_data.len()));
                // per-poly bbox (v5): the point-granular gate — a streaming
                // miss returns before touching any arc, preload reads instead
                // of recomputing. Rejects ~5% of poly-grid candidates for 4
                // compares (§15) — ~20x above the check's break-even.
                let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
                for &ri in poly {
                    for &r in &t.ring_refs[ri] {
                        for &(x, y) in &arcs_q[(r >> 1) as usize] {
                            bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
                        }
                    }
                }
                for v in [bb.0, bb.1, bb.2, bb.3] { push_fixed(&mut ring_data, v); }
                ring_data.extend_from_slice(&c16(poly.len()).to_le_bytes());
                for &ri in poly {
                    put_varint(&mut ring_data, t.ring_refs[ri].len() as u64);
                    for &r in &t.ring_refs[ri] { put_varint(&mut ring_data, u64::from(r)); }
                }
            }
        }
        poly_offsets.push(c32(ring_data.len()));
        for v in &poly_offsets { o.extend_from_slice(&v.to_le_bytes()); }
        o.extend_from_slice(&ring_data);
    }

    // ---- grid ----
    let grid_off = c32(o.len());
    stats.rings = grid_off - rings_off;
    o.extend_from_slice(&c16(g.ncols).to_le_bytes());
    o.extend_from_slice(&c16(g.nrows).to_le_bytes());
    for v in &csr.primary { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&c16(csr.uniq_lists).to_le_bytes());
    for v in &csr.list_offsets { o.extend_from_slice(&v.to_le_bytes()); }
    for v in &csr.list_ids { o.extend_from_slice(&v.to_le_bytes()); }

    stats.grid = c32(o.len()) - grid_off;

    for (i, off) in [arcs_off, rings_off, grid_off].into_iter().enumerate() {
        o[fixup + i * 4..fixup + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
    }
    Ok((o, stats))
}

/// Prepend the outer header, compressing the payload with `codec`.
///
/// # Errors
///
/// As [`compress`].
pub fn finish(payload: &[u8], codec: Codec) -> crate::Result<Vec<u8>> {
    let raw_len = c32(payload.len());
    let body = compress(payload, codec)?;
    let mut o = Vec::with_capacity(body.len() + OUTER_LEN);
    o.extend_from_slice(&MAGIC);
    o.push(VERSION);
    o.push(codec as u8);
    o.extend_from_slice(&raw_len.to_le_bytes());
    o.extend_from_slice(&[0u8; 2]); // reserved; pads the payload to +12 (v6)
    o.extend_from_slice(&body);
    Ok(o)
}

/// Compress `raw` with `codec` (body only, no outer header).
///
/// # Errors
///
/// [`Error::ZstdNotCompiled`] on `Codec::Zstd` when utz-encode was built
/// without the `zstd` feature; [`Error::Compress`]/[`Error::Xz`] if the
/// underlying compressor fails (not expected when writing to memory).
pub fn compress(raw: &[u8], codec: Codec) -> crate::Result<Vec<u8>> {
    let xz_err = |e| Error::Xz(format!("{e:?}"));
    Ok(match codec {
        Codec::Uncompressed => raw.to_vec(),
        Codec::Gzip => miniz_oxide::deflate::compress_to_vec_zlib(raw, 10),
        #[cfg(feature = "zstd")]
        Codec::Zstd => zstd::encode_all(raw, 22)?,
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => return Err(Error::ZstdNotCompiled),
        Codec::Brotli => {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams {
                quality: 11,
                lgwin: 24,
                ..Default::default()
            };
            brotli::BrotliCompress(&mut &raw[..], &mut out, &params)?;
            out
        }
        Codec::Xz => {
            use lzma_rust2::Write as _; // no_std lzma-rust2 (see utz/Cargo.toml)
            let bits = (usize::BITS - (raw.len().max(1) - 1).leading_zeros()).clamp(12, 26);
            let mut opts = lzma_rust2::XzOptions::with_preset(9);
            opts.lzma_options.dict_size = 1u32 << bits;
            // lzma-rust2 has no -9e helper; this is liblzma's extreme delta
            opts.lzma_options.nice_len = 273;
            opts.lzma_options.depth_limit = 512;
            let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).map_err(xz_err)?;
            w.write_all(raw).map_err(xz_err)?;
            w.finish().map_err(xz_err)?
        }
    })
}

fn zigzag(v: i64) -> u64 { ((v << 1) ^ (v >> 63)) as u64 }
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 { out.push(b); break; }        out.push(b | 0x80);
    }
}
