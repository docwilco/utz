//! Raw `extern "C"` surface for the webdist viewer's live container encode
//! (wasm32 only). Same no-bindgen style as utz-simplify/src/wasm.rs (whose
//! `utz_alloc`/`utz_simplify`/`utz_simplify_w` exports this cdylib links in).
//!
//! Stateful by design: the encode worker uploads the `<ds>.bin.z` blob once
//! (`utz_enc_init` parses the topology section written by
//! `utz_build::viz::dataset_bin`), then every parameter change is one cheap
//! `utz_enc_payload` call (simplify → quantize → clean → grid → serialize)
//! followed by one `utz_enc_compress` call per codec — so the JS can post
//! stats after every step instead of waiting for the slowest codec.
//! Cancellation is the worker's job: terminate + respawn + re-init.
//!
//! JS usage sketch (inside the encode worker):
//! ```js
//! const ptr = utz_enc_alloc(blob.byteLength);
//! new Uint8Array(memory.buffer).set(blob, ptr);
//! if (!utz_enc_init(ptr, blob.byteLength)) throw 'bad blob';   // frees ptr
//! const payloadLen = utz_enc_payload(algo, epsM, wMin, qbits, gridDeg);
//! const sections = [...Array(12)].map((_, i) => utz_enc_stat(i));
//! const brotliLen = utz_enc_compress(3);
//! ```

use crate::encode::{self, Codec, Params, PayloadStats};
use crate::topo::Topology;
use crate::{validate, Arc, Feat};
use utz_simplify::{simplify_weighted, DensityWeight, Simplify};

struct State {
    topo: Topology,
    /// per-vertex density (people/km², arc order) — empty when not shipped
    dens: Vec<f32>,
    /// tzid/offset metadata only (empty polys); geometry lives in `topo`
    feats: Vec<Feat>,
    dataset_code: u8,
    release: String,
    /// last utz_enc_payload result (input to utz_enc_compress)
    payload: Vec<u8>,
    stats: PayloadStats,
    /// last utz_enc_problems result: 12-byte records (see utz_enc_problems)
    problems: Vec<u8>,
}

// wasm32-unknown-unknown is single-threaded; one worker = one instance = one
// dataset. `static mut` keeps the no-bindgen ABI flat.
static mut STATE: Option<State> = None;

/// Allocate `n` bytes for the blob upload; `utz_enc_init` takes ownership.
#[no_mangle]
pub extern "C" fn utz_enc_alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n);
    let ptr = v.as_mut_ptr();
    core::mem::forget(v);
    ptr
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> { Some(self.take(1)?[0]) }
    fn u16(&mut self) -> Option<u16> { Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?)) }
    fn u32(&mut self) -> Option<u32> { Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
    fn f32(&mut self) -> Option<f32> { Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?)) }
    fn f64(&mut self) -> Option<f64> { Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?)) }
}

fn parse_blob(b: &[u8]) -> Option<State> {
    let mut r = Rd { b, p: 0 };
    if r.take(4)? != b"uTZv" {
        return None;
    }
    let flags = r.u32()?;
    if flags & 2 == 0 {
        return None; // no topology section (blob predates the live encode)
    }
    let n_arcs = r.u32()? as usize;
    let n_verts = r.u32()? as usize;
    let mut offs = Vec::with_capacity(n_arcs + 1);
    for _ in 0..=n_arcs {
        offs.push(r.u32()? as usize);
    }
    if *offs.last()? != n_verts {
        return None;
    }
    r.p = r.p.next_multiple_of(8);
    let mut arc_coords: Vec<Arc> = Vec::with_capacity(n_arcs);
    for a in 0..n_arcs {
        let mut arc = Vec::with_capacity(offs[a + 1] - offs[a]);
        for _ in offs[a]..offs[a + 1] {
            arc.push((r.f64()?, r.f64()?));
        }
        arc_coords.push(arc);
    }
    let mut dens = Vec::new();
    if flags & 1 != 0 {
        dens.reserve(n_verts);
        for _ in 0..n_verts {
            dens.push(r.f32()?);
        }
    }
    // ---- topology section (see viz::dataset_bin) ----
    let dataset_code = r.u8()?;
    let rel_len = r.u8()? as usize;
    let release = String::from_utf8(r.take(rel_len)?.to_vec()).ok()?;
    let n_features = r.u16()? as usize;
    let mut feats = Vec::with_capacity(n_features);
    for _ in 0..n_features {
        let offset = r.f32()? as f64;
        let len = r.u8()? as usize;
        let tzid = String::from_utf8(r.take(len)?.to_vec()).ok()?;
        feats.push(Feat { offset, tzid: (!tzid.is_empty()).then_some(tzid), polys: Vec::new() });
    }
    let n_rings = r.u32()? as usize;
    let mut ring_refs = Vec::with_capacity(n_rings);
    for _ in 0..n_rings {
        let n = r.u32()? as usize;
        let mut refs = Vec::with_capacity(n);
        for _ in 0..n {
            refs.push(r.u32()?);
        }
        ring_refs.push(refs);
    }
    let mut structure = Vec::with_capacity(n_features);
    for _ in 0..n_features {
        let npolys = r.u16()? as usize;
        let mut polys = Vec::with_capacity(npolys);
        for _ in 0..npolys {
            let nrings = r.u16()? as usize;
            let mut rings = Vec::with_capacity(nrings);
            for _ in 0..nrings {
                rings.push(r.u32()? as usize);
            }
            polys.push(rings);
        }
        structure.push(polys);
    }
    Some(State {
        topo: Topology { arc_coords, ring_refs, structure },
        dens,
        feats,
        dataset_code,
        release,
        payload: Vec::new(),
        stats: PayloadStats::default(),
        problems: Vec::new(),
    })
}

/// Parse a `<ds>.bin.z` blob (uTZv with the topology section) previously
/// copied into a `utz_enc_alloc(len)` buffer at `ptr`. Takes ownership of the
/// buffer. Returns 1 on success, 0 on a malformed/legacy blob.
///
/// # Safety
/// `ptr`/`len` must come from a single prior `utz_enc_alloc(len)` call whose
/// `len` bytes were fully initialized.
#[no_mangle]
pub unsafe extern "C" fn utz_enc_init(ptr: *mut u8, len: usize) -> u32 {
    let blob = Vec::from_raw_parts(ptr, len, len);
    let st = parse_blob(&blob);
    let ok = st.is_some();
    STATE = st;
    u32::from(ok)
}

/// Stage 1: simplify (algo ids as in utz-simplify/src/wasm.rs; ε in meters,
/// converted like the builder: /111 320, squared for Visvalingam) with
/// optional density weighting (`w_min < 1`, needs shipped densities), then
/// quantize → clean → grid → serialize via `payload_from_topology`. Returns
/// the payload length in bytes (0 = error / no init), stats via
/// [`utz_enc_stat`], the payload staying resident for [`utz_enc_compress`].
/// The simplify stage shared by [`utz_enc_payload`] and
/// [`utz_enc_problems`]. `pre_snap_bits` = Some(qbits) snaps every arc to
/// that grid BEFORE simplifying (the viewer's Q→S order); the later
/// quantize step then re-snaps the already-on-grid coords, a no-op.
fn simplified_arcs(st: &State, algo: u32, eps_m: f64, w_min: f64, pre_snap_bits: Option<u32>) -> Vec<Arc> {
    let eps_deg = eps_m / 111_320.0;
    let algo = match algo {
        0 => Simplify::Rdp { eps: eps_deg },
        1 => Simplify::Visvalingam { min_area: eps_deg * eps_deg },
        2 => Simplify::ImaiIri { eps: eps_deg },
        _ => Simplify::None,
    };
    let model = DensityWeight::new(w_min);
    let weighted = w_min < 1.0 && !st.dens.is_empty();
    let qmax = pre_snap_bits.map(|b| ((1u64 << (b - 1)) - 1) as f64);
    let mut base = 0usize;
    st.topo.arc_coords.iter().map(|a| {
        let snapped: Vec<(f64, f64)>;
        let input = match qmax {
            Some(q) => {
                snapped = a.iter()
                    .map(|&(x, y)| ((x / 180.0 * q).round() / q * 180.0, (y / 90.0 * q).round() / q * 90.0))
                    .collect();
                &snapped
            }
            None => a,
        };
        let out = if weighted {
            let w: Vec<f64> =
                st.dens[base..base + a.len()].iter().map(|&d| model.weight(d as f64)).collect();
            simplify_weighted(algo, input, &w)
        } else {
            utz_simplify::simplify(algo, input)
        };
        base += a.len();
        out
    }).collect()
}

#[no_mangle]
pub extern "C" fn utz_enc_payload(
    algo: u32,
    eps_m: f64,
    w_min: f64,
    quant_bits: u32,
    grid_deg: f64,
) -> u32 {
    let Some(st) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else { return 0 };
    let arcs = simplified_arcs(st, algo, eps_m, w_min, None);
    let p = Params {
        dataset: st.dataset_code,
        tzbb_release: &st.release,
        eps_m,
        quant_bits,
        grid_deg,
        codec: Codec::Uncompressed,
        geom: Default::default(),
        // same 0/1/2 byte convention as the viewer's algo knob
        simplify: match algo {
            1 => crate::encode::SimplifyAlgo::Visvalingam,
            2 => crate::encode::SimplifyAlgo::ImaiIri,
            _ => crate::encode::SimplifyAlgo::Rdp,
        },
    };
    match encode::payload_from_topology(&st.topo, &arcs, &st.feats, &p) {
        Ok((payload, stats)) => {
            st.stats = stats;
            st.payload = payload;
            st.payload.len() as u32
        }
        Err(_) => 0,
    }
}

/// Stats of the last [`utz_enc_payload`] (0 for an unknown index):
/// 0 header, 1 zone-table, 2 arc-store, 3 ring-index, 4 grid — section bytes;
/// 5 arcs, 6 verts (post-simplify+clean counts);
/// 7 dups, 8 spikes, 9 collinear, 10 rings dropped, 11 polys dropped,
/// 12 arcs dropped (cleanup removals).
#[no_mangle]
pub extern "C" fn utz_enc_stat(i: u32) -> u32 {
    let Some(st) = (unsafe { &*core::ptr::addr_of!(STATE) }) else { return 0 };
    let s = &st.stats;
    match i {
        0 => s.header,
        1 => s.zones,
        2 => s.arcs,
        3 => s.rings,
        4 => s.grid,
        5 => s.n_arcs,
        6 => s.n_verts,
        7 => s.clean.dups,
        8 => s.clean.spikes,
        9 => s.clean.collinear,
        10 => s.clean.rings_dropped,
        11 => s.clean.polys_dropped,
        12 => s.clean.arcs_dropped,
        _ => 0,
    }
}

/// Pointer to the resident payload of the last [`utz_enc_payload`] (whose
/// return value is its length; null if none) — lets the JS read the exact
/// bytes back, e.g. to offer a `.utz` download or diff against the builder.
#[no_mangle]
pub extern "C" fn utz_enc_payload_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) if !st.payload.is_empty() => st.payload.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// Locate problematic geometry (surviving ring self-crossings / collinear
/// overlaps) for the given knobs — the viewer's problems panel. Runs
/// simplify (Q→S when `pre` != 0: arcs snap to the `quant_bits` grid first)
/// → quantize → clean → drop, then sweeps every ring. Returns the record
/// count; records via [`utz_enc_problems_ptr`], 12 bytes each:
/// f32 lon | f32 lat | u16 kind (0 cross, 1 overlap) | u16 feature.
/// A spot on a shared border yields one record per owning ring — the JS
/// dedupes by location and joins the zone names.
#[no_mangle]
pub extern "C" fn utz_enc_problems(
    algo: u32,
    eps_m: f64,
    w_min: f64,
    quant_bits: u32,
    pre: u32,
) -> u32 {
    let Some(st) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else { return 0 };
    if !matches!(quant_bits, 16 | 24 | 32) {
        return 0;
    }
    let arcs = simplified_arcs(st, algo, eps_m, w_min, (pre != 0).then_some(quant_bits));
    let problems = validate::find_problems(&st.topo, &arcs, quant_bits);
    let mut out = Vec::with_capacity(problems.len() * 12);
    for p in &problems {
        out.extend_from_slice(&(p.lon as f32).to_le_bytes());
        out.extend_from_slice(&(p.lat as f32).to_le_bytes());
        let kind: u16 = match p.kind { validate::Kind::Cross => 0, validate::Kind::Overlap => 1 };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(p.feat as u16).to_le_bytes());
    }
    st.problems = out;
    problems.len() as u32
}

/// Pointer to the records of the last [`utz_enc_problems`] (null if none).
#[no_mangle]
pub extern "C" fn utz_enc_problems_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) if !st.problems.is_empty() => st.problems.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// tzid of feature `i` as (ptr, len) — for labelling problem records.
#[no_mangle]
pub extern "C" fn utz_enc_tzid_ptr(i: u32) -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) => st.feats.get(i as usize)
            .and_then(|f| f.tzid.as_deref())
            .map_or(core::ptr::null(), |s| s.as_ptr()),
        None => core::ptr::null(),
    }
}
#[no_mangle]
pub extern "C" fn utz_enc_tzid_len(i: u32) -> u32 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) => st.feats.get(i as usize)
            .and_then(|f| f.tzid.as_deref())
            .map_or(0, |s| s.len() as u32),
        None => 0,
    }
}

/// Stage 2: compress the resident payload with one codec byte (1 gzip/zlib,
/// 3 brotli, 4 xz — zstd is feature-gated off in the wasm build) and return
/// the compressed size in bytes; the shipped `.utz` adds a 10-byte outer
/// header. Returns 0 on error / unsupported codec / no payload.
#[no_mangle]
pub extern "C" fn utz_enc_compress(codec: u32) -> u32 {
    let Some(st) = (unsafe { &*core::ptr::addr_of!(STATE) }) else { return 0 };
    if st.payload.is_empty() {
        return 0;
    }
    let codec = match codec {
        1 => Codec::Gzip,
        3 => Codec::Brotli,
        4 => Codec::Xz,
        _ => return 0,
    };
    encode::compress(&st.payload, codec).map_or(0, |z| z.len() as u32)
}
