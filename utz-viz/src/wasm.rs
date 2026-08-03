//! Raw `extern "C"` surface for the webdist viewer's live container encode
//! and simplify-worker misassignment stats (wasm32 only). Same no-bindgen
//! style as utz-simplify/src/wasm.rs (whose
//! `utz_alloc`/`utz_simplify`/`utz_simplify_w` exports this cdylib links in).
//!
//! Stateful by design: the encode worker uploads the `<ds>.bin.z` blob once
//! (`utz_enc_init` parses the topology section written by
//! [`crate::emit::dataset_bin()`]), then every parameter change is one cheap
//! `utz_enc_payload` call (simplify → quantize → clean → grid → serialize)
//! followed by one `utz_enc_compress` call per codec, so the JS can post
//! stats after every step instead of waiting for the slowest codec.
//! Cancellation is the worker's job: terminate + respawn + re-init.
//!
//! JS usage sketch (inside the encode worker):
//! ```js
//! const ptr = utz_enc_alloc(blob.byteLength);
//! new Uint8Array(memory.buffer).set(blob, ptr);
//! if (!utz_enc_init(ptr, blob.byteLength)) throw 'bad blob';   // frees ptr
//! const payloadLen = utz_enc_payload(algo, epsM, wMin, qbits, gridDeg, geom);
//! const sections = [...Array(12)].map((_, i) => utz_enc_stat(i));
//! const brotliLen = utz_enc_compress(3);
//! ```
//!
//! The simplify worker's per-arc surface (`utz_ws_*`, backed by
//! [`crate::misassign`]) replaces what used to be JS math: upload one arc's
//! raw coords (and densities) into `utz_alloc` buffers, call [`utz_ws_arc`],
//! read the simplified coords back from the same buffer and the per-vertex
//! deviations via [`utz_ws_devs_ptr`]; the four misassignment accumulators
//! run across arcs ([`utz_ws_stat`]) until [`utz_ws_reset`].
//!
//! JS usage sketch (inside the simplify worker, per run):
//! ```js
//! utz_ws_reset();
//! for (each arc) {
//!   new Float64Array(memory.buffer, buf, n * 2).set(rawXy);
//!   if (dens) new Float64Array(memory.buffer, dbuf, n).set(dens);
//!   const k = utz_ws_arc(algo, epsDeg, wMin, quantCode, pre, buf, n, dens ? dbuf : 0);
//!   const xy = new Float64Array(memory.buffer, buf, k * 2);
//!   const devs = new Float32Array(memory.buffer, utz_ws_devs_ptr(), k);
//! }
//! const [area, people, qarea, qpeople] = [0, 1, 2, 3].map(utz_ws_stat);
//! ```

use crate::misassign::{self, Acc, ArcParams, Quant};
use utz_encode::encode::{self, Codec, GeomEncoding, Params, PayloadStats};
use utz_encode::topo::Topology;
use utz_encode::{validate, Arc, Feat};
use utz_simplify::{simplify_weighted, DensityWeight, Simplify};

struct State {
    topo: Topology,
    /// per-vertex density (people/km², arc order); empty when not shipped
    dens: Vec<f32>,
    /// tzid/offset metadata only (empty polys); geometry lives in `topo`
    feats: Vec<Feat>,
    dataset_code: u8,
    release: String,
    /// last `utz_enc_payload` result (input to `utz_enc_compress`)
    payload: Vec<u8>,
    stats: PayloadStats,
    /// last `utz_enc_problems` result: 12-byte records (see `utz_enc_problems`)
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
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
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
    if flags & 4 != 0 {
        r.u32()?; // raw ring-coordinate count: prefix-only, the JS reads it
    }
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
        let offset = f64::from(r.f32()?);
        let len = r.u8()? as usize;
        let tzid = String::from_utf8(r.take(len)?.to_vec()).ok()?;
        feats.push(Feat {
            offset,
            tzid: (!tzid.is_empty()).then_some(tzid),
            polys: Vec::new(),
        });
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
        let n_polys = r.u16()? as usize;
        let mut polys = Vec::with_capacity(n_polys);
        for _ in 0..n_polys {
            let n_poly_rings = r.u16()? as usize;
            let mut rings = Vec::with_capacity(n_poly_rings);
            for _ in 0..n_poly_rings {
                rings.push(r.u32()? as usize);
            }
            polys.push(rings);
        }
        structure.push(polys);
    }
    Some(State {
        topo: Topology {
            arc_coords,
            ring_refs,
            structure,
        },
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
    #[expect(
        clippy::same_length_and_capacity,
        reason = "the buffer comes from utz_enc_alloc's Vec::with_capacity(len), so len is the true capacity; the suggested slice copy would leak that allocation"
    )]
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
fn simplified_arcs(
    st: &State,
    algo: u32,
    eps_m: f64,
    w_min: f64,
    pre_snap_bits: Option<u32>,
) -> Vec<Arc> {
    let algo = utz_encode::encode::to_simplify(
        u8::try_from(algo)
            .ok()
            .and_then(utz_encode::encode::SimplifyAlgo::from_byte)
            .unwrap_or(utz_encode::encode::SimplifyAlgo::None),
        eps_m / 111_320.0,
    );
    let model = DensityWeight::new(w_min);
    let weighted = w_min < 1.0 && !st.dens.is_empty();
    #[expect(
        clippy::cast_precision_loss,
        reason = "qmax = 2^(quant_bits-1)-1 ≤ 2^31-1, exact in f64"
    )]
    let qmax = pre_snap_bits.map(|b| ((1u64 << (b - 1)) - 1) as f64);
    let mut base = 0usize;
    st.topo
        .arc_coords
        .iter()
        .map(|a| {
            let snapped: Vec<(f64, f64)>;
            let input = match qmax {
                Some(q) => {
                    snapped = a
                        .iter()
                        .map(|&(x, y)| {
                            (
                                (x / 180.0 * q).round() / q * 180.0,
                                (y / 90.0 * q).round() / q * 90.0,
                            )
                        })
                        .collect();
                    &snapped
                }
                None => a,
            };
            let out = if weighted {
                let w: Vec<f64> = st.dens[base..base + a.len()]
                    .iter()
                    .map(|&d| model.weight(f64::from(d)))
                    .collect();
                simplify_weighted(algo, input, &w)
            } else {
                utz_simplify::simplify(algo, input)
            };
            base += a.len();
            out
        })
        .collect()
}

/// A buffer length heading back over the JS boundary as `u32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "usize is 32 bits on wasm32, so lengths cross the JS boundary losslessly"
)]
fn len_u32(n: usize) -> u32 {
    n as u32
}

/// `geom` is a [`GeomEncoding`] header byte (0 varint arcs, 1 fixed-width
/// arcs, 2 full rings, 3 coarse); unknown values fall back to varint arcs.
#[no_mangle]
pub extern "C" fn utz_enc_payload(
    algo: u32,
    eps_m: f64,
    w_min: f64,
    quant_bits: u32,
    grid_deg: f64,
    geom: u32,
) -> u32 {
    let Some(st) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else {
        return 0;
    };
    let arcs = simplified_arcs(st, algo, eps_m, w_min, None);
    let p = Params {
        dataset: st.dataset_code,
        tzbb_release: &st.release,
        eps_m,
        quant_bits,
        grid_deg,
        codec: Codec::Uncompressed,
        geom: match geom {
            1 => GeomEncoding::FixedWidthArcs,
            2 => GeomEncoding::FullRings,
            3 => GeomEncoding::Coarse,
            _ => GeomEncoding::VarintArcs,
        },
        // the viewer's algo knob sends SimplifyAlgo header bytes
        simplify: u8::try_from(algo)
            .ok()
            .and_then(utz_encode::encode::SimplifyAlgo::from_byte)
            .unwrap_or(utz_encode::encode::SimplifyAlgo::Rdp),
        density_weight_floor: (w_min < 1.0).then_some(w_min),
    };
    match encode::payload_from_topology(&st.topo, &arcs, &st.feats, &p) {
        Ok((payload, stats)) => {
            st.stats = stats;
            st.payload = payload;
            len_u32(st.payload.len())
        }
        Err(_) => 0,
    }
}

/// Stats of the last [`utz_enc_payload`] (0 for an unknown index):
/// 0 header, 1 zone-table, 2 arc-store, 3 ring-index, 4 grid (section bytes);
/// 5 arcs, 6 verts (post-simplify+clean counts);
/// 7 dups, 8 spikes, 9 collinear, 10 rings dropped, 11 polys dropped,
/// 12 arcs dropped (cleanup removals).
#[no_mangle]
pub extern "C" fn utz_enc_stat(i: u32) -> u32 {
    let Some(st) = (unsafe { &*core::ptr::addr_of!(STATE) }) else {
        return 0;
    };
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
/// return value is its length; null if none), letting the JS read the exact
/// bytes back, e.g. to offer a `.utz` download or diff against the builder.
#[no_mangle]
pub extern "C" fn utz_enc_payload_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) if !st.payload.is_empty() => st.payload.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// Locate problematic geometry (surviving ring self-crossings / collinear
/// overlaps) for the given knobs: the viewer's problems panel. Runs
/// simplify (Q→S when `pre` != 0: arcs snap to the `quant_bits` grid first)
/// → quantize → clean → drop, then sweeps every ring. Returns the record
/// count; records via [`utz_enc_problems_ptr`], 12 bytes each:
/// f32 lon | f32 lat | u16 kind (0 cross, 1 overlap) | u16 feature.
/// A spot on a shared border yields one record per owning ring; the JS
/// dedupes by location and joins the zone names.
#[no_mangle]
pub extern "C" fn utz_enc_problems(
    algo: u32,
    eps_m: f64,
    w_min: f64,
    quant_bits: u32,
    pre: u32,
) -> u32 {
    let Some(st) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else {
        return 0;
    };
    if !matches!(quant_bits, 16 | 24 | 32) {
        return 0;
    }
    let arcs = simplified_arcs(st, algo, eps_m, w_min, (pre != 0).then_some(quant_bits));
    let problems = validate::find_problems(&st.topo, &arcs, quant_bits);
    let mut out = Vec::with_capacity(problems.len() * 12);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "record layout: lon/lat narrow to f32 for display, and feature indices fit u16 because the blob header stores n_features as a u16"
    )]
    for p in &problems {
        out.extend_from_slice(&(p.lon as f32).to_le_bytes());
        out.extend_from_slice(&(p.lat as f32).to_le_bytes());
        let kind: u16 = match p.kind {
            validate::Kind::Cross => 0,
            validate::Kind::Overlap => 1,
        };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(p.feat as u16).to_le_bytes());
    }
    st.problems = out;
    len_u32(problems.len())
}

/// Pointer to the records of the last [`utz_enc_problems`] (null if none).
#[no_mangle]
pub extern "C" fn utz_enc_problems_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) if !st.problems.is_empty() => st.problems.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// tzid of feature `i` as (ptr, len), for labelling problem records.
#[no_mangle]
pub extern "C" fn utz_enc_tzid_ptr(i: u32) -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) => st
            .feats
            .get(i as usize)
            .and_then(|f| f.tzid.as_deref())
            .map_or(core::ptr::null(), str::as_ptr),
        None => core::ptr::null(),
    }
}
#[no_mangle]
pub extern "C" fn utz_enc_tzid_len(i: u32) -> u32 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(st) => st
            .feats
            .get(i as usize)
            .and_then(|f| f.tzid.as_deref())
            .map_or(0, |s| len_u32(s.len())),
        None => 0,
    }
}

/// Stage 2: compress the resident payload with one codec byte (1 gzip/zlib,
/// 3 brotli, 4 xz; zstd is feature-gated off in the wasm build) and return
/// the compressed size in bytes; the shipped `.utz` adds a 10-byte outer
/// header. Returns 0 on error / unsupported codec / no payload.
#[no_mangle]
pub extern "C" fn utz_enc_compress(codec: u32) -> u32 {
    let Some(st) = (unsafe { &*core::ptr::addr_of!(STATE) }) else {
        return 0;
    };
    if st.payload.is_empty() {
        return 0;
    }
    let codec = match codec {
        1 => Codec::Gzip,
        3 => Codec::Brotli,
        4 => Codec::Xz,
        _ => return 0,
    };
    encode::compress(&st.payload, codec).map_or(0, |z| len_u32(z.len()))
}

/// Simplify-worker misassignment state: the last arc's deviations plus the
/// accumulators running across arcs (single-threaded wasm32, same `static
/// mut` convention as `STATE`).
struct WsState {
    deviations: Vec<f32>,
    acc: Acc,
    qacc: Acc,
}

static mut WS: WsState = WsState {
    deviations: Vec::new(),
    acc: Acc {
        area: 0.0,
        people: 0.0,
    },
    qacc: Acc {
        area: 0.0,
        people: 0.0,
    },
};

/// Zero the four running misassignment accumulators (start of a run).
#[no_mangle]
pub extern "C" fn utz_ws_reset() {
    let ws = unsafe { &mut *core::ptr::addr_of_mut!(WS) };
    ws.acc = Acc::default();
    ws.qacc = Acc::default();
    ws.deviations.clear();
}

/// One arc through the whole simplify-worker pipeline
/// ([`misassign::arc_misassign()`]): pre-snap when `pre` != 0 (Q→S), simplify
/// (`algo` ids and `param` as in `utz_simplify`; density-weighted when
/// `dens` is non-null and `w_min` < 1), pocket + display-snap pricing into
/// the running accumulators. `quant` codes are the viewer's quant-knob
/// indices (0 f64, 1 f32, 2 i32, 3 i24, 4 i16). Returns the kept count:
/// the simplified coords overwrite the first `kept * 2` doubles of `xy`
/// (already display-snapped in Q→S order, raw copies otherwise), and the
/// per-kept-vertex deviations are readable via [`utz_ws_devs_ptr`].
///
/// # Safety
/// `xy` must point at `n_pts * 2` valid doubles and `dens` at `n_pts` valid
/// doubles or be null (e.g. from `utz_alloc`).
#[no_mangle]
pub unsafe extern "C" fn utz_ws_arc(
    algo: u32,
    param: f64,
    w_min: f64,
    quant: u32,
    pre: u32,
    xy: *mut f64,
    n_pts: usize,
    dens: *const f64,
) -> usize {
    let ws = &mut *core::ptr::addr_of_mut!(WS);
    let buf = core::slice::from_raw_parts_mut(xy, n_pts * 2);
    let arc: Vec<(f64, f64)> = buf.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let dens = (!dens.is_null()).then(|| core::slice::from_raw_parts(dens, n_pts));
    let params = ArcParams {
        // same ids as utz-simplify's wasm exports; unknown ids pass through
        algo: match algo {
            1 => Simplify::Rdp { eps: param },
            2 => Simplify::Visvalingam { min_area: param },
            3 => Simplify::ImaiIri { eps: param },
            _ => Simplify::None,
        },
        density_weight_floor: w_min,
        quant: Quant::from_code(quant),
        pre: pre != 0,
    };
    let r = misassign::arc_misassign(&arc, dens, &params, &mut ws.acc, &mut ws.qacc);
    for (i, (x, y)) in r.kept.iter().enumerate() {
        buf[i * 2] = *x;
        buf[i * 2 + 1] = *y;
    }
    ws.deviations = r.deviations;
    r.kept.len()
}

/// Pointer to the last [`utz_ws_arc`]'s deviations (one f32 per kept
/// vertex; null if none). `devs[v]` prices the output segment `v-1`→`v`; 0
/// at the arc start and wherever nothing was dropped.
#[no_mangle]
pub extern "C" fn utz_ws_devs_ptr() -> *const f32 {
    let ws = unsafe { &*core::ptr::addr_of!(WS) };
    if ws.deviations.is_empty() {
        core::ptr::null()
    } else {
        ws.deviations.as_ptr()
    }
}

/// A running misassignment accumulator (0 for an unknown index):
/// 0 simplification area (km²), 1 simplification people,
/// 2 display-snap area (km²), 3 display-snap people.
#[no_mangle]
pub extern "C" fn utz_ws_stat(i: u32) -> f64 {
    let ws = unsafe { &*core::ptr::addr_of!(WS) };
    match i {
        0 => ws.acc.area,
        1 => ws.acc.people,
        2 => ws.qacc.area,
        3 => ws.qacc.people,
        _ => 0.0,
    }
}
