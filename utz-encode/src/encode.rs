//! Self-describing container serializer (PLAN.md §4/§5 step 7).
//!
//! Layout (all little-endian):
//! ```text
//! outer:  magic "uTZ1" | version u8 | codec u8 | raw_len u32 | payload…
//!         (raw_len = UNCOMPRESSED payload size, so decoders allocate once)
//! payload (compressed per codec):
//!   header:     dataset u8 | quant_bits u8 | grid_deg f32 | eps_m f32
//!               | tzbb_release (len u8 + bytes)
//!               | n_features u16 | arcs_off u32 | rings_off u32 | grid_off u32
//!   zone table: str_offsets u16[n_features+1] | tzid pool bytes   (zone i = feature i)
//!   arc store:  n_arcs u32 | arc_offsets u32[n_arcs+1] (relative to arc data)
//!               | per arc: varint vcount | first vertex i{16,24,32}×2
//!               | zigzag-varint deltas
//!   ring index: feat_offsets u32[n_features+1] (relative to ring data)
//!               | per feature: npolys u16; per poly: bbox i{16,24,32}×4
//!               | nrings u16; per ring: varint nrefs | varint signed arc refs
//!               (id<<1|rev)
//!   grid:       ncols u16 | nrows u16 | primary u16[ncols*nrows]
//!               | uniq u16 | list_offsets u16[uniq+1] | list_ids u16[Σ]
//! ```
//! The header records every knob, so the runtime decoder stays generic (§4).
//! Grid + bboxes are derived from the QUANTIZED geometry, so what the runtime
//! PIPs is exactly what the grid indexed.


use crate::grid::{self, Order};
use crate::{clean, topo, Feat};

// on-disk magic stays ASCII ("μ" is 2 bytes in UTF-8 and byte literals
// reject non-ASCII); the project brands as μTZ, the container as uTZ1
pub const MAGIC: [u8; 4] = *b"uTZ1";
pub const VERSION: u8 = 1;

#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(u8)]
pub enum Codec {
    Uncompressed = 0,
    Gzip = 1,
    Zstd = 2,
    Brotli = 3,
    Xz = 4,
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
pub fn encode(feats: &[Feat], p: &Params) -> anyhow::Result<Vec<u8>> {
    Ok(finish(build_payload(feats, p)?, p.codec))
}

/// Everything but the outer header + compression (so size sweeps can compress
/// one payload with several codecs).
pub fn build_payload(feats: &[Feat], p: &Params) -> anyhow::Result<Vec<u8>> {
    let t = topo::build_topology(feats, p.eps_m / 111_320.0);
    Ok(payload_from_topology(&t, &t.arc_coords, feats, p)?.0)
}

/// Serialize an already-simplified topology: quantize → grid → sections.
/// `arc_coords` may differ from `t.arc_coords` (the wasm viewer simplifies
/// per-arc itself); `feats` supplies only per-feature metadata (tzid, offset)
/// — geometry comes from the arcs. `p.eps_m` is recorded in the header, not
/// applied.
pub fn payload_from_topology(
    t: &topo::Topology,
    arc_coords: &[Vec<(f64, f64)>],
    feats: &[Feat],
    p: &Params,
) -> anyhow::Result<(Vec<u8>, PayloadStats)> {
    anyhow::ensure!(matches!(p.quant_bits, 16 | 24 | 32), "quant_bits must be 16/24/32");
    anyhow::ensure!((0.1..=45.0).contains(&p.grid_deg), "grid_deg must be within 0.1–45");
    anyhow::ensure!(feats.len() < 0x7FFF, "feature count exceeds 15-bit zone ids");
    let qmax = ((1u64 << (p.quant_bits - 1)) - 1) as f64;
    let qx = |lon: f64| (lon / 180.0 * qmax).round() as i32;
    let qy = |lat: f64| (lat / 90.0 * qmax).round() as i32;
    let dq = |v: i32, half: f64| v as f64 / qmax * half;

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

    // grid over the dequantized geometry = exactly what the runtime sees
    let arcs_dq: Vec<Vec<(f64, f64)>> = arcs_q.iter()
        .map(|a| a.iter().map(|&(x, y)| (dq(x, 180.0), dq(y, 90.0))).collect())
        .collect();
    let quantized = t.reconstruct(feats, &arcs_dq);
    let g = grid::build(&quantized, p.grid_deg, 8);
    let areas = grid::feat_areas(&quantized);
    let csr = grid::intern_csr(&g, Order::CellDominantFirst, &areas);

    let mut stats = PayloadStats {
        n_arcs: arcs_q.len() as u32,
        n_verts: arcs_q.iter().map(|a| a.len() as u32).sum(),
        clean: cst,
        ..Default::default()
    };
    let mut o = Vec::new();
    // ---- header ----
    o.push(p.dataset);
    o.push(p.quant_bits as u8);
    o.extend_from_slice(&(p.grid_deg as f32).to_le_bytes());
    o.extend_from_slice(&(p.eps_m as f32).to_le_bytes());
    anyhow::ensure!(p.tzbb_release.len() < 256, "tzbb_release too long");
    o.push(p.tzbb_release.len() as u8);
    o.extend_from_slice(p.tzbb_release.as_bytes());
    o.extend_from_slice(&(feats.len() as u16).to_le_bytes());
    let fixup = o.len(); // arcs_off, rings_off, grid_off patched below
    o.extend_from_slice(&[0u8; 12]);
    stats.header = o.len() as u32;

    // ---- zone table (zone i = feature i) ----
    let mut str_off: Vec<u16> = Vec::with_capacity(feats.len() + 1);
    let mut pool: Vec<u8> = Vec::new();
    for f in feats {
        str_off.push(pool.len() as u16);
        pool.extend_from_slice(f.tzid.as_deref().unwrap_or("").as_bytes());
    }
    str_off.push(pool.len() as u16);
    for v in &str_off { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&pool);

    // ---- arc store ----
    let arcs_off = o.len() as u32;
    stats.zones = arcs_off - stats.header;
    let push_fixed = |o: &mut Vec<u8>, v: i32| {
        let n = (p.quant_bits as usize + 7) / 8;
        o.extend_from_slice(&v.to_le_bytes()[0..n]);
    };
    o.extend_from_slice(&(arcs_q.len() as u32).to_le_bytes());
    let mut arc_data = Vec::new();
    let mut arc_offsets: Vec<u32> = Vec::with_capacity(arcs_q.len() + 1);
    for a in &arcs_q {
        arc_offsets.push(arc_data.len() as u32);
        put_varint(&mut arc_data, a.len() as u64);
        let (mut px, mut py) = (0i64, 0i64);
        for (i, &(x, y)) in a.iter().enumerate() {
            if i == 0 {
                push_fixed(&mut arc_data, x);
                push_fixed(&mut arc_data, y);
            } else {
                put_varint(&mut arc_data, zigzag(x as i64 - px));
                put_varint(&mut arc_data, zigzag(y as i64 - py));
            }
            (px, py) = (x as i64, y as i64);
        }
    }
    arc_offsets.push(arc_data.len() as u32);
    for v in &arc_offsets { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&arc_data);

    // ---- ring index (per-poly bbox from quantized arcs, for lazy-mode skip) ----
    let rings_off = o.len() as u32;
    stats.arcs = rings_off - arcs_off;
    let mut ring_data = Vec::new();
    let mut feat_offsets: Vec<u32> = Vec::with_capacity(feats.len() + 1);
    for fi in 0..feats.len() {
        feat_offsets.push(ring_data.len() as u32);
        ring_data.extend_from_slice(&(t.structure[fi].len() as u16).to_le_bytes());
        for poly in &t.structure[fi] {
            let mut bb = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
            for &ri in poly {
                for &r in &t.ring_refs[ri] {
                    for &(x, y) in &arcs_q[(r >> 1) as usize] {
                        bb = (bb.0.min(x), bb.1.min(y), bb.2.max(x), bb.3.max(y));
                    }
                }
            }
            for v in [bb.0, bb.1, bb.2, bb.3] { push_fixed(&mut ring_data, v); }
            ring_data.extend_from_slice(&(poly.len() as u16).to_le_bytes());
            for &ri in poly {
                put_varint(&mut ring_data, t.ring_refs[ri].len() as u64);
                for &r in &t.ring_refs[ri] { put_varint(&mut ring_data, r as u64); }
            }
        }
    }
    feat_offsets.push(ring_data.len() as u32);
    for v in &feat_offsets { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&ring_data);

    // ---- grid ----
    let grid_off = o.len() as u32;
    stats.rings = grid_off - rings_off;
    o.extend_from_slice(&(g.ncols as u16).to_le_bytes());
    o.extend_from_slice(&(g.nrows as u16).to_le_bytes());
    for v in &csr.primary { o.extend_from_slice(&v.to_le_bytes()); }
    o.extend_from_slice(&(csr.uniq_lists as u16).to_le_bytes());
    for v in &csr.list_offsets { o.extend_from_slice(&v.to_le_bytes()); }
    for v in &csr.list_ids { o.extend_from_slice(&v.to_le_bytes()); }

    stats.grid = o.len() as u32 - grid_off;

    for (i, off) in [arcs_off, rings_off, grid_off].into_iter().enumerate() {
        o[fixup + i * 4..fixup + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
    }
    Ok((o, stats))
}

/// Prepend the outer header, compressing the payload with `codec`.
pub fn finish(payload: Vec<u8>, codec: Codec) -> Vec<u8> {
    let raw_len = payload.len() as u32;
    let body = compress(&payload, codec);
    let mut o = Vec::with_capacity(body.len() + 10);
    o.extend_from_slice(&MAGIC);
    o.push(VERSION);
    o.push(codec as u8);
    o.extend_from_slice(&raw_len.to_le_bytes());
    o.extend_from_slice(&body);
    o
}

pub fn compress(raw: &[u8], codec: Codec) -> Vec<u8> {
    match codec {
        Codec::Uncompressed => raw.to_vec(),
        Codec::Gzip => miniz_oxide::deflate::compress_to_vec_zlib(raw, 10),
        #[cfg(feature = "zstd")]
        Codec::Zstd => zstd::encode_all(raw, 22).expect("zstd"),
        #[cfg(not(feature = "zstd"))]
        Codec::Zstd => panic!("utz-encode built without the `zstd` feature"),
        Codec::Brotli => {
            let mut out = Vec::new();
            let mut params = brotli::enc::BrotliEncoderParams::default();
            params.quality = 11;
            params.lgwin = 24;
            brotli::BrotliCompress(&mut &raw[..], &mut out, &params).expect("brotli");
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
            let mut w = lzma_rust2::XzWriter::new(Vec::new(), opts).expect("xz");
            w.write_all(raw).expect("xz");
            w.finish().expect("xz")
        }
    }
}

fn zigzag(v: i64) -> u64 { ((v << 1) ^ (v >> 63)) as u64 }
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 { out.push(b); break; } else { out.push(b | 0x80); }
    }
}
