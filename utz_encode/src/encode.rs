//! Self-describing container serializer.
//!
//! Layout (all little-endian):
//! ```text
//! prologue: magic "uTZ1" | version u8 | 3 reserved bytes — format identity,
//!           the only layout frozen across versions
//! header:   one fixed [`PayloadHeader`] record (utz_common), PLAINTEXT —
//!           codec, decompressed size, every section offset and count; the
//!           encoder Pwrites it, the reader Preads it (before decompressing
//!           anything), so the field list IS the wire layout
//! sections (compressed per the header's codec):
//!   zone table: str_offsets u16[n_features+1] | tzid pool bytes   (zone i = feature i)
//!   arc store:  arc_offsets u32[n_arcs+1] (relative to arc data)
//!               | per arc: varint vcount | first vertex i{16,24,32}×2
//!               | zigzag-varint deltas
//!   ring index: parent u16[n_polys] + poly_offsets u32[n_polys+1]
//!   (relative to ring data) — grid candidates are polys
//!               | per feature: npolys u16; per poly: bbox i{16,24,32}×4
//!               | nrings u16; per ring: varint nrefs | varint signed arc refs
//!               (id<<1|rev)
//!   grid:       primary u16[ncols*nrows]
//!               | list_offsets u16[uniq+1] | list_ids u16[Σ]
//!   release:    the TZBB release string bytes (the payload tail)
//! ```
//! The header records every knob, so the runtime decoder stays generic.
//! Grid + bboxes are derived from the QUANTIZED geometry, so what the runtime
//! PIPs is exactly what the grid indexed.

use scroll::{Pread, Pwrite, LE};
use utz_common::{Dataset, PayloadHeader, QuantBits, PAYLOAD_HEADER_LEN, PROLOGUE_LEN};
pub use utz_common::{MAGIC, VERSION};

use crate::error::ensure;
use crate::grid::{self, Order};
use crate::{clean, q_lat, q_lon, qmax_for, topo, Arc, Error, Feat};
/// Checked narrowing for serializer counts/offsets: the format stores these
/// at fixed width and a wrap would silently corrupt the container, so panic.
/// Data-dependent limits (feature count, tzid pool, CSR tables) are
/// `ensure!`-guarded with [`Error::FormatLimit`] before these run.
fn c32(n: usize) -> u32 {
    u32::try_from(n).expect("exceeds u32 format width")
}
fn c16(n: usize) -> u16 {
    u16::try_from(n).expect("exceeds u16 format width")
}

pub use utz_common::{Codec, GeomEncoding, SimplifyAlgo};

/// ε-driven `Simplify` for the topology builder: ε is a max deviation in
/// degrees for RDP / Imai–Iri; Visvalingam's area threshold is derived as
/// ε². The viewer uses the same convention, so parameters found in the
/// viewer plug straight into build scripts.
#[must_use]
pub fn to_simplify(algo: SimplifyAlgo, eps_deg: f64) -> utz_simplify::Simplify {
    match algo {
        SimplifyAlgo::None => utz_simplify::Simplify::None,
        SimplifyAlgo::Rdp => utz_simplify::Simplify::Rdp { eps: eps_deg },
        SimplifyAlgo::ImaiIri => utz_simplify::Simplify::ImaiIri { eps: eps_deg },
        SimplifyAlgo::Visvalingam => utz_simplify::Simplify::Visvalingam {
            min_area: eps_deg * eps_deg,
        },
    }
}

/// Everything one encode run needs to know: the recipe knobs plus the
/// provenance recorded in the asset's header.
#[derive(Clone, Copy)]
pub struct Params<'a> {
    /// The dataset code byte: bits 0–1 zone set (0 = now, 1 = 1970,
    /// 2 = all/comprehensive), bit 2 set = land-only (clear = with-oceans).
    pub dataset: u8,
    /// TZBB release tag recorded in the header (rules snapshot / cache key)
    pub tzbb_release: &'a str,
    pub eps_m: f64,
    /// 16 / 24 / 32
    pub quant_bits: u32,
    /// grid cell size in degrees, 0.1–45; fractional (0.5, 4/3, …) allowed
    pub grid_deg: f64,
    pub codec: Codec,
    /// simplification algorithm: applied by [`build_payload`],
    /// recorded in the header either way
    pub simplify: SimplifyAlgo,
    /// arc-store encoding: delta+varint (default) or fixed-width
    pub geom: GeomEncoding,
    /// Population-density weight floor, recorded in the header as
    /// provenance (`None` = unweighted). The weighting itself is applied
    /// by the caller's simplification pass (`utz_build`'s
    /// `encode_weighted()`); this only stamps the knob.
    pub density_weight_floor: Option<f64>,
}

/// Byte size of each payload section + post-simplification geometry counts:
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
/// yourself (`topo::build_topology_weighted()`) and use
/// [`payload_from_topology`] + [`finish`] (see `utz_build`'s wrapper).
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
/// As [`payload_from_topology`].
pub fn build_payload(feats: &[Feat], p: &Params) -> crate::Result<Vec<u8>> {
    let algo = to_simplify(p.simplify, p.eps_m / 111_320.0);
    let t = topo::build_topology_algo(feats, algo);
    Ok(payload_from_topology(&t, &t.arc_coords, feats, p)?.0)
}

/// The header's fixed-point (1e-4) density-weight floor. A real floor
/// never stamps 0 (which means "unweighted"), and `w >= 1.0` means
/// weighting off, which callers express as `None`.
///
/// # Errors
/// [`Error::DensityWeightFloor`] for a non-finite floor or one outside
/// (0, 1).
fn density_weight_floor_e4(floor: Option<f64>) -> crate::Result<u16> {
    match floor {
        None => Ok(0),
        Some(w) => {
            ensure!(
                w.is_finite() && w > 0.0 && w < 1.0,
                Error::DensityWeightFloor { floor: w }
            );
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "validated into (0, 1) above, so the product is in (0, 10_000)"
            )]
            Ok(((w * 1e4).round() as u16).max(1))
        }
    }
}

/// Serialize an already-simplified topology: quantize → grid → sections.
/// `arc_coords` may differ from `t.arc_coords` (the wasm viewer simplifies
/// per-arc itself); `feats` supplies only per-feature metadata (tzid,
/// offset). Geometry comes from the arcs. `p.eps_m` is recorded in the
/// header, not applied.
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
/// [`Error::FormatLimit`] guard applies (payload over 4 GiB, unreachable
/// for real datasets).
pub fn payload_from_topology(
    t: &topo::Topology,
    arc_coords: &[Arc],
    feats: &[Feat],
    p: &Params,
) -> crate::Result<(Vec<u8>, PayloadStats)> {
    ensure!(
        matches!(p.quant_bits, 16 | 24 | 32),
        Error::QuantBits { bits: p.quant_bits }
    );
    ensure!(
        (0.1..=45.0).contains(&p.grid_deg),
        Error::GridDeg { deg: p.grid_deg }
    );
    let density_weight_floor_e4 = density_weight_floor_e4(p.density_weight_floor)?;
    ensure!(
        feats.len() < 0x7FFF,
        Error::FormatLimit {
            what: "feature count (15-bit zone ids)",
            n: feats.len(),
            max: 0x7FFE
        }
    );
    let qmax = qmax_for(p.quant_bits);
    let geom = quantize_clean(t, arc_coords, qmax);
    let (g, csr, parent) = poly_grid(&geom, feats, p, qmax)?;
    let full_rings =
        (p.geom == GeomEncoding::FullRings).then(|| flatten_full_rings(&geom, feats.len()));

    let mut stats = PayloadStats {
        n_arcs: c32(geom.arcs_q.len()),
        n_verts: geom.arcs_q.iter().map(|a| c32(a.len())).sum(),
        clean: geom.stats,
        ..Default::default()
    };
    let counts = eager_counts(
        full_rings.as_ref(),
        &geom,
        feats.len(),
        parent.len(),
        p.geom,
    );
    ensure_header_limits(p, counts, parent.len())?;
    let (eager_coords, eager_rings, eager_polys) = counts;
    // header space reserved up front (plaintext; finish() compresses only
    // the sections after it); stored offsets are relative to the sections
    let mut o = vec![0u8; PAYLOAD_HEADER_LEN];
    stats.header = c32(o.len());
    write_zone_table(&mut o, feats)?;
    let (arcs_off, rings_off) = write_geometry_sections(
        &mut o,
        p,
        &geom,
        full_rings.as_ref(),
        &parent,
        feats.len(),
        &mut stats,
    );

    let grid_off = c32(o.len());
    stats.rings = grid_off - rings_off;
    write_grid(&mut o, &csr);
    stats.grid = c32(o.len()) - grid_off;

    let release_off = c32(o.len());
    o.extend_from_slice(p.tzbb_release.as_bytes());

    #[expect(clippy::cast_possible_truncation, reason = "f32 header fields")]
    let (grid_deg, eps_m) = (p.grid_deg as f32, p.eps_m as f32);
    let header_len = c32(PAYLOAD_HEADER_LEN);
    let header = PayloadHeader {
        arcs_off: arcs_off - header_len,
        rings_off: rings_off - header_len,
        grid_off: grid_off - header_len,
        release_off: release_off - header_len,
        eager_coords: u32::try_from(eager_coords).expect("guarded by ensure_header_limits"),
        eager_rings,
        eager_polys,
        // no arc store outside the arc-encoded geometries
        n_arcs: match p.geom {
            GeomEncoding::VarintArcs | GeomEncoding::FixedWidthArcs => stats.n_arcs,
            GeomEncoding::FullRings | GeomEncoding::Coarse => 0,
        },
        raw_len: c32(o.len() - PAYLOAD_HEADER_LEN),
        grid_deg,
        eps_m,
        n_features: c16(feats.len()),
        ncols: c16(g.ncols()),
        nrows: c16(g.nrows()),
        uniq: c16(csr.uniq_lists),
        release_len: c16(p.tzbb_release.len()),
        flags: 0, // reserved
        dataset: Dataset::from_byte(p.dataset).expect("guarded by ensure_header_limits"),
        quant_bits: QuantBits::from_byte(
            u8::try_from(p.quant_bits).expect("quant_bits guarded to 16/24/32"),
        )
        .expect("quant_bits guarded to 16/24/32"),
        simplify_algo: p.simplify,
        geom: p.geom,
        codec: Codec::Uncompressed, // finish() records the actual codec
        density_weight_floor_e4,
        reserved: 0,
    };
    o.pwrite_with(header, 0, LE)
        .expect("header buffer reserved at PAYLOAD_HEADER_LEN");
    Ok((o, stats))
}

/// Cleaned, quantized geometry: the topology whose rings/structure survived
/// the degenerate-drop, plus the quantized shared arcs they reference.
struct CleanGeom {
    /// `arc_coords` empty: geometry lives in `arcs_q`
    t: topo::Topology,
    arcs_q: Vec<Arc<i32>>,
    stats: clean::CleanStats,
}

/// Quantize arcs at `qmax`, then clean the snapping artifacts per shared arc
/// (dups, zero-area spikes, collinear pass-throughs) and drop rings that
/// collapsed to zero area (see clean.rs). Junction endpoints stay put, so
/// neighbouring zones remain stitched.
fn quantize_clean(t: &topo::Topology, arc_coords: &[Arc], qmax: f64) -> CleanGeom {
    let mut stats = clean::CleanStats::default();
    let arcs_q: Vec<Arc<i32>> = arc_coords
        .iter()
        .map(|a| {
            let mut q: Arc<i32> = a
                .iter()
                .map(|&(lon, lat)| (q_lon(lon, qmax), q_lat(lat, qmax)))
                .collect();
            let closed = a.len() > 1 && a.first() == a.last();
            clean::clean_arc(&mut q, closed, &mut stats);
            q
        })
        .collect();
    let (ring_refs, structure, arcs_q) =
        clean::drop_degenerate_rings(&t.ring_refs, &t.structure, arcs_q, &mut stats);
    CleanGeom {
        t: topo::Topology {
            arc_coords: Vec::new(),
            ring_refs,
            structure,
        },
        arcs_q,
        stats,
    }
}

/// Grid over the snapped (dequantized) geometry = exactly what the runtime
/// sees. Rasterized per POLYGON, not per feature: border-cell candidate
/// lists carry poly ids, so lookups jump straight to the ~2 polys whose rings
/// touch the cell instead of bbox-scanning every poly of every candidate
/// feature (the `polygrid_probe` bench: 20-24 polys parsed → 2.1, per-poly bboxes
/// redundant, CSR growth ≈ dropped bbox bytes). Returns the cell grid, the
/// interned CSR (interior cells rewritten to FEATURE ids: coarse answers
/// need no parent hop; border lists keep poly ids), and the poly→feature
/// parent table.
fn poly_grid(
    geom: &CleanGeom,
    feats: &[Feat],
    p: &Params,
    qmax: f64,
) -> crate::Result<(grid::CellGrid, grid::Csr, Vec<u16>)> {
    let dq = |v: i32, half: f64| f64::from(v) / qmax * half;
    let arcs_snapped: Vec<Arc> = geom
        .arcs_q
        .iter()
        .map(|a| {
            a.iter()
                .map(|&(x, y)| (dq(x, 180.0), dq(y, 90.0)))
                .collect()
        })
        .collect();
    let snapped = geom.t.reconstruct(feats, &arcs_snapped);
    let mut poly_feats: Vec<Feat> = Vec::new();
    let mut parent: Vec<u16> = Vec::new(); // poly id -> feature id
    for (fi, qf) in snapped.iter().enumerate() {
        for poly in &qf.polys {
            poly_feats.push(Feat {
                offset: 0.0,
                tzid: None,
                polys: vec![poly.clone()],
            });
            parent.push(c16(fi));
        }
    }
    ensure!(
        poly_feats.len() < 0x7FFF,
        Error::FormatLimit {
            what: "polygon count (15-bit ids)",
            n: poly_feats.len(),
            max: 0x7FFE
        }
    );
    let g = grid::build(&poly_feats, p.grid_deg, 8);
    let areas = grid::feat_areas(&poly_feats);
    let mut csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);
    for v in &mut csr.primary {
        if *v & 0x8000 == 0 && *v != 0x7FFF {
            *v = parent[*v as usize];
        }
    }
    // the format's CSR tables are u16: border-cell tags carry a 15-bit
    // list index, and list_offsets index into list_ids as u16 — a fine grid
    // on a dense dataset can overflow both. Fail loudly instead of the `as
    // u16` wrap silently corrupting the tables.
    ensure!(
        csr.uniq_lists < 0x7FFF,
        Error::GridLists {
            deg: p.grid_deg,
            n: csr.uniq_lists
        }
    );
    ensure!(
        u16::try_from(csr.list_ids.len()).is_ok(),
        Error::GridListIds {
            deg: p.grid_deg,
            n: csr.list_ids.len()
        }
    );
    Ok((g, csr, parent))
}

/// The flattened preload-cache shape (geom=2): per-ring coord runs +
/// ring/poly index tables. Built before the header so the eager counts are
/// exact and serialization just copies.
struct FullRingsSections {
    coords: Vec<(i32, i32)>,
    ring_ends: Vec<u32>,
    /// per poly: bbox + `ring_ends` index one past its last ring
    polys: Vec<([i32; 4], u32)>,
}

/// Assemble the [`FullRingsSections`]. Junction dedup stays within a ring.
fn flatten_full_rings(geom: &CleanGeom, nfeats: usize) -> FullRingsSections {
    let (mut coords, mut ring_ends, mut polys) = (Vec::new(), Vec::new(), Vec::new());
    for fi in 0..nfeats {
        for poly in &geom.t.structure[fi] {
            let mut bb = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
            for &ri in poly {
                let rstart = coords.len();
                for &r in &geom.t.ring_refs[ri] {
                    let (id, rev) = ((r >> 1) as usize, (r & 1) == 1);
                    let seg = coords.len();
                    let a = &geom.arcs_q[id];
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
            polys.push((bb, c32(ring_ends.len())));
        }
    }
    FullRingsSections {
        coords,
        ring_ends,
        polys,
    }
}

/// Eager-cache counts the header advertises: what preload will hold.
/// Exact for `FullRingsSections` (they locate the sections) and `Coarse`; for the
/// arc-store geoms a reservation: coords as Σ referenced-arc vcounts
/// (junction dedup at decode shrinks it a hair; a reservation may only
/// over-estimate).
fn eager_counts(
    full_rings: Option<&FullRingsSections>,
    geom: &CleanGeom,
    nfeats: usize,
    n_parent: usize,
    g: GeomEncoding,
) -> (u64, u32, u32) {
    match full_rings {
        Some(rings) => (
            rings.coords.len() as u64,
            c32(rings.ring_ends.len()),
            c32(rings.polys.len()),
        ),
        // coarse: no geometry — polys counts the parent table entries
        None if g == GeomEncoding::Coarse => (0, 0, c32(n_parent)),
        None => {
            let mut coords: u64 = 0;
            let (mut rings, mut polys) = (0u32, 0u32);
            for fi in 0..nfeats {
                for poly in &geom.t.structure[fi] {
                    polys += 1;
                    for &ri in poly {
                        rings += 1;
                        coords += geom.t.ring_refs[ri]
                            .iter()
                            .map(|&r| geom.arcs_q[(r >> 1) as usize].len() as u64)
                            .sum::<u64>();
                    }
                }
            }
            (coords, rings, polys)
        }
    }
}

/// The geometry-dependent sections between the zone table and the grid;
/// returns `(arcs_off, rings_off)`.
fn write_geometry_sections(
    o: &mut Vec<u8>,
    p: &Params,
    geom: &CleanGeom,
    full_rings: Option<&FullRingsSections>,
    parent: &[u16],
    n_features: usize,
    stats: &mut PayloadStats,
) -> (u32, u32) {
    let (arcs_off, rings_off);
    if p.geom == GeomEncoding::Coarse {
        // ---- coarse (geom=3): no geometry sections, just the parent table
        // (border-cell candidate poly ids still resolve to features) ----
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        rings_off = c32(o.len());
        for &pf in parent {
            o.extend_from_slice(&pf.to_le_bytes());
        }
    } else if let Some(rings) = full_rings {
        // ---- full-rings geometry (geom=2): coords 4-aligned within the
        // payload (the 12-byte outer header keeps it 4-aligned in flash) ----
        while !o.len().is_multiple_of(4) {
            o.push(0);
        }
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        write_full_rings(o, rings, p.quant_bits);
        // ---- ring index reduces to the parent table ----
        rings_off = c32(o.len());
        stats.arcs = rings_off - arcs_off;
        for &pf in parent {
            o.extend_from_slice(&pf.to_le_bytes());
        }
    } else {
        arcs_off = c32(o.len());
        stats.zones = arcs_off - stats.header;
        write_arc_store(o, &geom.arcs_q, p.geom, p.quant_bits);
        rings_off = c32(o.len());
        stats.arcs = rings_off - arcs_off;
        for &pf in parent {
            o.extend_from_slice(&pf.to_le_bytes());
        }
        write_ring_index(o, geom, n_features, parent.len(), p.quant_bits);
    }
    (arcs_off, rings_off)
}

/// Format-limit guards for the header fields (the header itself is written
/// by `Pwrite` at the end of `payload_from_topology`, once offsets exist).
fn ensure_header_limits(p: &Params, counts: (u64, u32, u32), n_parent: usize) -> crate::Result<()> {
    let (eager_coords, _, eager_polys) = counts;
    ensure!(
        u32::try_from(eager_coords).is_ok(),
        Error::FormatLimit {
            what: "eager_coords",
            n: usize::try_from(eager_coords).unwrap_or(usize::MAX),
            max: u32::MAX as usize
        }
    );
    ensure!(
        eager_polys as usize == n_parent,
        Error::FormatLimit {
            what: "eager_polys (must equal parent count)",
            n: eager_polys as usize,
            max: n_parent
        }
    );
    ensure!(
        u16::try_from(p.tzbb_release.len()).is_ok(),
        Error::FormatLimit {
            what: "tzbb_release bytes",
            n: p.tzbb_release.len(),
            max: u16::MAX as usize
        }
    );
    ensure!(
        Dataset::from_byte(p.dataset).is_some(),
        Error::FormatLimit {
            what: "dataset byte (zone set 0-2, land-only bit)",
            n: p.dataset as usize,
            max: 0b110
        }
    );
    Ok(())
}

/// Zone table: `str_offsets u16[n+1]` + tzid pool (zone i = feature i).
fn write_zone_table(o: &mut Vec<u8>, feats: &[Feat]) -> crate::Result<()> {
    // the offsets are u16 on disk — fail loudly instead of the `as u16`
    // wrap silently corrupting the table (same policy as the CSR guards)
    let total_pool: usize = feats
        .iter()
        .map(|f| f.tzid.as_deref().unwrap_or("").len())
        .sum();
    ensure!(
        u16::try_from(total_pool).is_ok(),
        Error::FormatLimit {
            what: "tzid pool bytes (u16 offsets)",
            n: total_pool,
            max: u16::MAX as usize
        }
    );
    let mut str_off: Vec<u16> = Vec::with_capacity(feats.len() + 1);
    let mut pool: Vec<u8> = Vec::new();
    for f in feats {
        str_off.push(c16(pool.len()));
        pool.extend_from_slice(f.tzid.as_deref().unwrap_or("").as_bytes());
    }
    str_off.push(c16(pool.len()));
    for v in &str_off {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&pool);
    Ok(())
}

/// Fixed-width coord at the quant width (i16→2 B, i24→3 B, i32→4 B).
fn push_fixed(o: &mut Vec<u8>, v: i32, quant_bits: u32) {
    let n = (quant_bits as usize).div_ceil(8);
    o.extend_from_slice(&v.to_le_bytes()[0..n]);
}

/// Full-rings sections: `[coords][ring_ends u32][polys bbox 4×i32 + rend
/// u32]`, with coords at quant width (i16 4 B/vertex, i24 packed 6 B,
/// i32 8 B).
fn write_full_rings(o: &mut Vec<u8>, rings: &FullRingsSections, quant_bits: u32) {
    for &(x, y) in &rings.coords {
        push_fixed(o, x, quant_bits);
        push_fixed(o, y, quant_bits);
    }
    for v in &rings.ring_ends {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for &(bb, rend) in &rings.polys {
        for v in bb {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&rend.to_le_bytes());
    }
}

/// Arc store: `arc_offsets u32[n_arcs+1] | per arc: varint vcount |
/// first vertex fixed | zigzag-varint deltas` (or all-fixed for
/// `GeomEncoding::FixedWidthArcs`); `n_arcs` lives in the payload header.
fn write_arc_store(o: &mut Vec<u8>, arcs_q: &[Arc<i32>], geom: GeomEncoding, quant_bits: u32) {
    let mut arc_data = Vec::new();
    let mut arc_offsets: Vec<u32> = Vec::with_capacity(arcs_q.len() + 1);
    for a in arcs_q {
        arc_offsets.push(c32(arc_data.len()));
        put_varint(&mut arc_data, a.len() as u64);
        match geom {
            GeomEncoding::VarintArcs => {
                let (mut px, mut py) = (0i64, 0i64);
                for (i, &(x, y)) in a.iter().enumerate() {
                    if i == 0 {
                        push_fixed(&mut arc_data, x, quant_bits);
                        push_fixed(&mut arc_data, y, quant_bits);
                    } else {
                        put_varint(&mut arc_data, zigzag(i64::from(x) - px));
                        put_varint(&mut arc_data, zigzag(i64::from(y) - py));
                    }
                    (px, py) = (i64::from(x), i64::from(y));
                }
            }
            GeomEncoding::FixedWidthArcs => {
                for &(x, y) in a {
                    push_fixed(&mut arc_data, x, quant_bits);
                    push_fixed(&mut arc_data, y, quant_bits);
                }
            }
            GeomEncoding::FullRings | GeomEncoding::Coarse => {
                unreachable!("handled by the caller")
            }
        }
    }
    arc_offsets.push(c32(arc_data.len()));
    for v in &arc_offsets {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&arc_data);
}

/// Ring index (per-poly records: grid candidates are polys; the parent
/// table maps them to features): `poly_offsets u32[n+1] | per poly: bbox
/// fixed×4 | nrings u16 | per ring: varint nrefs + signed arc refs`.
fn write_ring_index(
    o: &mut Vec<u8>,
    geom: &CleanGeom,
    nfeats: usize,
    n_polys: usize,
    quant_bits: u32,
) {
    let mut ring_data = Vec::new();
    let mut poly_offsets: Vec<u32> = Vec::with_capacity(n_polys + 1);
    for fi in 0..nfeats {
        for poly in &geom.t.structure[fi] {
            poly_offsets.push(c32(ring_data.len()));
            // per-poly bbox: the point-granular gate — a streaming
            // miss returns before touching any arc, preload reads instead
            // of recomputing. Rejects ~5% of poly-grid candidates for 4
            // compares — ~20x above the check's break-even.
            let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &ri in poly {
                for &r in &geom.t.ring_refs[ri] {
                    for &(x, y) in &geom.arcs_q[(r >> 1) as usize] {
                        bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
                    }
                }
            }
            for v in [bb.0, bb.1, bb.2, bb.3] {
                push_fixed(&mut ring_data, v, quant_bits);
            }
            ring_data.extend_from_slice(&c16(poly.len()).to_le_bytes());
            for &ri in poly {
                put_varint(&mut ring_data, geom.t.ring_refs[ri].len() as u64);
                for &r in &geom.t.ring_refs[ri] {
                    put_varint(&mut ring_data, u64::from(r));
                }
            }
        }
    }
    poly_offsets.push(c32(ring_data.len()));
    for v in &poly_offsets {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&ring_data);
}

/// Grid tables: `primary u16[ncols*nrows] | list_offsets u16[uniq+1] |
/// list_ids u16[Σ]`; the dimensions and `uniq` live in the payload header.
fn write_grid(o: &mut Vec<u8>, csr: &grid::Csr) {
    for v in &csr.primary {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in &csr.list_offsets {
        o.extend_from_slice(&v.to_le_bytes());
    }
    for v in &csr.list_ids {
        o.extend_from_slice(&v.to_le_bytes());
    }
}

/// Prepend the outer header, compressing the payload with `codec`.
///
/// # Errors
///
/// As [`compress`].
///
/// # Panics
/// If `payload` is shorter than the header record or its header bytes are
/// invalid: `payload` must come from [`build_payload`] /
/// [`payload_from_topology`].
pub fn finish(payload: &[u8], codec: Codec) -> crate::Result<Vec<u8>> {
    // the header stays plaintext; only the sections after it compress
    let (header_bytes, sections) = payload.split_at(PAYLOAD_HEADER_LEN);
    let mut header: PayloadHeader = header_bytes
        .pread_with(0, LE)
        .expect("build_payload wrote a valid header");
    header.codec = codec;
    let body = compress(sections, codec)?;
    let mut o = Vec::with_capacity(PROLOGUE_LEN + PAYLOAD_HEADER_LEN + body.len());
    o.extend_from_slice(&MAGIC);
    o.push(VERSION);
    o.extend_from_slice(&[0u8; 3]); // reserved; pads the header start to +8
    o.resize(PROLOGUE_LEN + PAYLOAD_HEADER_LEN, 0);
    o.pwrite_with(header, PROLOGUE_LEN, LE)
        .expect("header space reserved");
    o.extend_from_slice(&body);
    Ok(o)
}

/// Compress `raw` with `codec` (body only, no outer header).
///
/// # Errors
///
/// [`Error::ZstdNotCompiled`] on `Codec::Zstd` when `utz_encode` was built
/// without the `zstd` feature; [`Error::Compress`]/[`Error::Xz`] if the
/// underlying compressor fails (not expected when writing to memory).
pub fn compress(raw: &[u8], codec: Codec) -> crate::Result<Vec<u8>> {
    let xz_err = |e| Error::Xz(format!("{e:?}"));
    Ok(match codec {
        Codec::Uncompressed => raw.to_vec(),
        Codec::Gzip => miniz_oxide::deflate::compress_to_vec_zlib(raw, 10),
        #[cfg(feature = "zstd")]
        Codec::Zstd => {
            use std::io::Write as _;
            // Never declare a window larger than the content (decoders
            // allocate the declared window), and cap at 2^26: ruzstd
            // (utz's pure-Rust backend) refuses frames declaring over
            // 100 MB. encode_all would let level 22 default to 2^27.
            let log = raw.len().next_power_of_two().trailing_zeros().clamp(10, 26);
            let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 22)?;
            enc.set_pledged_src_size(Some(raw.len() as u64))?;
            enc.window_log(log)?;
            enc.write_all(raw)?;
            enc.finish()?
        }
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

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)).cast_unsigned()
}
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}
