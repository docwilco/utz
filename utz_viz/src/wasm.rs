//! The raw `extern "C"` surface for the webdist viewer's live container
//! encode and simplify-worker misassignment stats (wasm32 only). It keeps
//! the same no-bindgen style as `utz_simplify/src/wasm.rs`, whose
//! `utz_alloc()`/`utz_simplify()`/`utz_simplify_w()` exports this cdylib
//! links in.
//!
//! The surface is stateful by design: the encode worker uploads the
//! `<ds>.bin.z` blob once (`utz_enc_init()` parses the topology section
//! written by [`crate::emit::dataset_bin()`]), and then every parameter
//! change is one cheap `utz_enc_payload()` call (simplify → quantize →
//! clean → grid → serialize) followed by one `utz_enc_compress()` call per
//! codec, so the JS can post stats after every step instead of waiting for
//! the slowest codec. Cancellation is the worker's job: it terminates,
//! respawns, and re-inits the instance.
//!
//! The encode worker's JS uses the surface roughly like this:
//! ```js
//! const ptr = utz_enc_alloc(blob.byteLength);
//! new Uint8Array(memory.buffer).set(blob, ptr);
//! if (!utz_enc_init(ptr, blob.byteLength)) throw 'bad blob';   // frees ptr
//! const payloadLen = utz_enc_payload(algorithm, epsilonM, wMin, qbits, gridDeg, geom);
//! const sections = [...Array(12)].map((_, i) => utz_enc_stat(i));
//! const brotliLen = utz_enc_compress(3);
//! ```
//!
//! The simplify worker's per-arc surface (`utz_ws_*`, backed by
//! [`crate::misassign`]) replaces what used to be JS math: the worker
//! uploads one arc's raw coords (and densities) into buffers from
//! `utz_alloc()`, calls [`utz_ws_arc()`], and reads the simplified coords
//! back from the same buffer and the per-vertex deviations via
//! [`utz_ws_devs_ptr()`]; the four misassignment accumulators run across
//! arcs ([`utz_ws_stat()`]) until [`utz_ws_reset()`].
//!
//! The simplify worker's JS uses it roughly like this, once per run:
//! ```js
//! utz_ws_reset();
//! for (each arc) {
//!   new Float64Array(memory.buffer, buf, n * 2).set(rawXy);
//!   if (dens) new Float64Array(memory.buffer, dbuf, n).set(dens);
//!   const k = utz_ws_arc(algorithm, epsilonDeg, wMin, quantCode, pre, buf, n, dens ? dbuf : 0);
//!   const xy = new Float64Array(memory.buffer, buf, k * 2);
//!   const devs = new Float32Array(memory.buffer, utz_ws_devs_ptr(), k);
//! }
//! const [area, people, qarea, qpeople] = [0, 1, 2, 3].map(utz_ws_stat);
//! ```

use crate::misassign::{self, Acc, ArcParams, Quant};
use utz_encode::encode::{self, Codec, Dataset, GeomEncoding, Params, PayloadStats};
use utz_encode::topo::Topology;
use utz_encode::{validate, Arc, Feat};
use utz_simplify::{simplify_weighted, DensityWeight, Simplify};

struct State {
    topo: Topology,
    /// The per-vertex densities (people/km², in arc order); the vector is
    /// empty when the blob did not ship densities.
    densities: Vec<f32>,
    /// The features carry tzid/offset metadata only (their polys are
    /// empty); the geometry lives in `topo`.
    feats: Vec<Feat>,
    dataset: Dataset,
    release: String,
    /// The result of the last `utz_enc_payload()` call, which is the
    /// input to `utz_enc_compress()`.
    payload: Vec<u8>,
    stats: PayloadStats,
    /// The result of the last `utz_enc_problems()` call, stored as the
    /// 12-byte records that `utz_enc_problems()` documents.
    problems: Vec<u8>,
}

// wasm32-unknown-unknown is single-threaded; one worker = one instance = one
// dataset. `static mut` keeps the no-bindgen ABI flat.
static mut STATE: Option<State> = None;

/// Allocates `n` bytes for the blob upload; `utz_enc_init()` takes
/// ownership.
#[no_mangle]
pub extern "C" fn utz_enc_alloc(n: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(n);
    let ptr = buffer.as_mut_ptr();
    core::mem::forget(buffer);
    ptr
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.bytes.get(self.position..self.position + n)?;
        self.position += n;
        Some(slice)
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

fn parse_blob(bytes: &[u8]) -> Option<State> {
    let mut reader = Reader { bytes, position: 0 };
    if reader.take(4)? != b"uTZv" {
        return None;
    }
    let flags = reader.u32()?;
    if flags & 2 == 0 {
        return None; // no topology section (blob predates the live encode)
    }
    let n_arcs = reader.u32()? as usize;
    let n_verts = reader.u32()? as usize;
    if flags & 4 != 0 {
        reader.u32()?; // raw ring-coordinate count: prefix-only, the JS reads it
    }
    let mut offsets = Vec::with_capacity(n_arcs + 1);
    for _ in 0..=n_arcs {
        offsets.push(reader.u32()? as usize);
    }
    if *offsets.last()? != n_verts {
        return None;
    }
    reader.position = reader.position.next_multiple_of(8);
    let mut arc_coords: Vec<Arc> = Vec::with_capacity(n_arcs);
    for arc_index in 0..n_arcs {
        let mut arc = Vec::with_capacity(offsets[arc_index + 1] - offsets[arc_index]);
        for _ in offsets[arc_index]..offsets[arc_index + 1] {
            arc.push((reader.f64()?, reader.f64()?));
        }
        arc_coords.push(arc);
    }
    let mut densities = Vec::new();
    if flags & 1 != 0 {
        densities.reserve(n_verts);
        for _ in 0..n_verts {
            densities.push(reader.f32()?);
        }
    }
    // ---- topology section (see viz::dataset_bin) ----
    let dataset = Dataset::from_byte(reader.u8()?)?;
    let release_len = reader.u8()? as usize;
    let release = String::from_utf8(reader.take(release_len)?.to_vec()).ok()?;
    let n_features = reader.u16()? as usize;
    let mut feats = Vec::with_capacity(n_features);
    for _ in 0..n_features {
        let offset = f64::from(reader.f32()?);
        let len = reader.u8()? as usize;
        let tzid = String::from_utf8(reader.take(len)?.to_vec()).ok()?;
        feats.push(Feat {
            offset,
            tzid: (!tzid.is_empty()).then_some(tzid),
            polys: Vec::new(),
        });
    }
    let n_rings = reader.u32()? as usize;
    let mut ring_refs = Vec::with_capacity(n_rings);
    for _ in 0..n_rings {
        let n_refs = reader.u32()? as usize;
        let mut refs = Vec::with_capacity(n_refs);
        for _ in 0..n_refs {
            refs.push(reader.u32()?);
        }
        ring_refs.push(refs);
    }
    let mut structure = Vec::with_capacity(n_features);
    for _ in 0..n_features {
        let n_polys = reader.u16()? as usize;
        let mut polys = Vec::with_capacity(n_polys);
        for _ in 0..n_polys {
            let n_poly_rings = reader.u16()? as usize;
            let mut rings = Vec::with_capacity(n_poly_rings);
            for _ in 0..n_poly_rings {
                rings.push(reader.u32()? as usize);
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
        densities,
        feats,
        dataset,
        release,
        payload: Vec::new(),
        stats: PayloadStats::default(),
        problems: Vec::new(),
    })
}

/// Parses a `<ds>.bin.z` blob (uTZv with the topology section) previously
/// copied into a `utz_enc_alloc(len)` buffer at `ptr`. Takes ownership of
/// the buffer. Returns 1 on success and 0 on a malformed or legacy blob.
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
    let state = parse_blob(&blob);
    let ok = state.is_some();
    STATE = state;
    u32::from(ok)
}

/// The simplify stage shared by [`utz_enc_payload()`] and
/// [`utz_enc_problems()`]. The `algorithm` ids are as in
/// `utz_simplify/src/wasm.rs`, ε arrives in meters and is converted the
/// way the builder converts it (divided by 111 320, and squared for
/// Visvalingam), and density weighting is optional (`w_min < 1`, which
/// needs shipped densities). Passing `pre_snap_bits` as `Some(qbits)`
/// snaps every arc to that grid BEFORE simplifying (the viewer's Q→S
/// order); the later quantize step then re-snaps the already-on-grid
/// coords, which is a no-op.
fn simplified_arcs(
    state: &State,
    algorithm: u32,
    epsilon_m: f64,
    w_min: f64,
    pre_snap_bits: Option<u32>,
) -> Vec<Arc> {
    let algorithm = utz_encode::encode::to_simplify(
        u8::try_from(algorithm)
            .ok()
            .and_then(utz_encode::encode::SimplifyAlgorithm::from_byte)
            .unwrap_or(utz_encode::encode::SimplifyAlgorithm::None),
        epsilon_m / utz_encode::METERS_PER_DEG,
    );
    let model = DensityWeight::new(w_min);
    let weighted = w_min < 1.0 && !state.densities.is_empty();
    let qmax = pre_snap_bits.map(utz_encode::qmax_for);
    let mut base = 0usize;
    state
        .topo
        .arc_coords
        .iter()
        .map(|arc| {
            let snapped: Vec<(f64, f64)>;
            let input = match qmax {
                Some(quant_max) => {
                    snapped = arc
                        .iter()
                        .map(|&(x, y)| {
                            (
                                utz_encode::dq_lon(
                                    f64::from(utz_encode::q_lon(x, quant_max)),
                                    quant_max,
                                ),
                                utz_encode::dq_lat(
                                    f64::from(utz_encode::q_lat(y, quant_max)),
                                    quant_max,
                                ),
                            )
                        })
                        .collect();
                    &snapped
                }
                None => arc,
            };
            let out = if weighted {
                let weights: Vec<f64> = state.densities[base..base + arc.len()]
                    .iter()
                    .map(|&density| model.weight(f64::from(density)))
                    .collect();
                simplify_weighted(algorithm, input, &weights)
            } else {
                utz_simplify::simplify(algorithm, input)
            };
            base += arc.len();
            out
        })
        .collect()
}

/// Narrows a buffer length to the `u32` that heads back over the JS
/// boundary.
#[expect(
    clippy::cast_possible_truncation,
    reason = "usize is 32 bits on wasm32, so lengths cross the JS boundary losslessly"
)]
fn len_u32(n: usize) -> u32 {
    n as u32
}

/// Runs stage 1 of the live encode: simplify, then quantize → clean →
/// grid → serialize via `payload_from_topology()`. Returns the payload
/// length in bytes (0 on error or before a successful init); the stats
/// become readable via [`utz_enc_stat()`], and the payload stays resident
/// for [`utz_enc_compress()`].
///
/// `geom` is a [`GeomEncoding`] header byte (0 varint arcs, 1 fixed-width
/// arcs, 2 full rings, 3 coarse); unknown values fall back to varint arcs.
#[no_mangle]
pub extern "C" fn utz_enc_payload(
    algorithm: u32,
    epsilon_m: f64,
    w_min: f64,
    quant_bits: u32,
    grid_deg: f64,
    geom: u32,
) -> u32 {
    let Some(state) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else {
        return 0;
    };
    let arcs = simplified_arcs(state, algorithm, epsilon_m, w_min, None);
    let params = Params {
        dataset: state.dataset,
        tzbb_release: &state.release,
        epsilon_m,
        quant_bits,
        grid_deg,
        codec: Codec::Uncompressed,
        // the viewer's geom knob sends GeomEncoding header bytes; an
        // unknown byte is an error, not a silent varint-arcs fallback
        geom: match u8::try_from(geom).ok().and_then(GeomEncoding::from_byte) {
            Some(geom) => geom,
            None => return 0,
        },
        // the viewer's algorithm knob sends SimplifyAlgorithm header bytes
        simplify: u8::try_from(algorithm)
            .ok()
            .and_then(utz_encode::encode::SimplifyAlgorithm::from_byte)
            .unwrap_or(utz_encode::encode::SimplifyAlgorithm::Rdp),
        density_weight_floor: (w_min < 1.0).then_some(w_min),
    };
    match encode::payload_from_topology(&state.topo, &arcs, &state.feats, &params) {
        Ok((payload, payload_stats)) => {
            state.stats = payload_stats;
            state.payload = payload;
            len_u32(state.payload.len())
        }
        Err(_) => 0,
    }
}

/// Returns one stat of the last [`utz_enc_payload()`] call (0 for an
/// unknown index). Indices 0-4 are section byte counts: 0 header,
/// 1 zone-table, 2 arc-store, 3 ring-index, 4 grid. Indices 5 and 6 are
/// the post-simplify+clean counts of arcs and verts. Indices 7-12 are the
/// cleanup removals: 7 dups, 8 spikes, 9 collinear, 10 rings dropped,
/// 11 polys dropped, 12 arcs dropped.
#[no_mangle]
pub extern "C" fn utz_enc_stat(i: u32) -> u32 {
    let Some(state) = (unsafe { &*core::ptr::addr_of!(STATE) }) else {
        return 0;
    };
    let payload_stats = &state.stats;
    match i {
        0 => payload_stats.header,
        1 => payload_stats.zones,
        2 => payload_stats.arcs,
        3 => payload_stats.rings,
        4 => payload_stats.grid,
        5 => payload_stats.n_arcs,
        6 => payload_stats.n_verts,
        7 => payload_stats.clean.dups,
        8 => payload_stats.clean.spikes,
        9 => payload_stats.clean.collinear,
        10 => payload_stats.clean.rings_dropped,
        11 => payload_stats.clean.polys_dropped,
        12 => payload_stats.clean.arcs_dropped,
        _ => 0,
    }
}

/// Returns a pointer to the resident payload of the last
/// [`utz_enc_payload()`] call (whose return value is its length; null when
/// there is none). This lets the JS read the exact bytes back, e.g. to
/// offer a `.utz` download or diff against the builder.
#[no_mangle]
pub extern "C" fn utz_enc_payload_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(state) if !state.payload.is_empty() => state.payload.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// Locates problematic geometry (surviving ring self-crossings and
/// collinear overlaps) for the given knobs, which backs the viewer's
/// problems panel. It runs simplify (Q→S when `pre` != 0, so arcs snap to
/// the `quant_bits` grid first) → quantize → clean → drop, and then
/// sweeps every ring. Returns the record count; the records are readable
/// via [`utz_enc_problems_ptr()`], 12 bytes each:
/// f32 lon | f32 lat | u16 kind (0 cross, 1 overlap) | u16 feature.
/// A spot on a shared border yields one record per owning ring; the JS
/// dedupes by location and joins the zone names.
#[no_mangle]
pub extern "C" fn utz_enc_problems(
    algorithm: u32,
    epsilon_m: f64,
    w_min: f64,
    quant_bits: u32,
    pre: u32,
) -> u32 {
    let Some(state) = (unsafe { &mut *core::ptr::addr_of_mut!(STATE) }) else {
        return 0;
    };
    if !matches!(quant_bits, 16 | 24 | 32) {
        return 0;
    }
    let arcs = simplified_arcs(
        state,
        algorithm,
        epsilon_m,
        w_min,
        (pre != 0).then_some(quant_bits),
    );
    let problems = validate::find_problems(&state.topo, &arcs, quant_bits);
    let mut out = Vec::with_capacity(problems.len() * 12);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "record layout: lon/lat narrow to f32 for display, and feature indices fit u16 because the blob header stores n_features as a u16"
    )]
    for problem in &problems {
        out.extend_from_slice(&(problem.lon as f32).to_le_bytes());
        out.extend_from_slice(&(problem.lat as f32).to_le_bytes());
        let kind: u16 = match problem.kind {
            validate::Kind::Cross => 0,
            validate::Kind::Overlap => 1,
        };
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&(problem.feat as u16).to_le_bytes());
    }
    state.problems = out;
    len_u32(problems.len())
}

/// Returns a pointer to the records of the last [`utz_enc_problems()`]
/// call (null when there are none).
#[no_mangle]
pub extern "C" fn utz_enc_problems_ptr() -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(state) if !state.problems.is_empty() => state.problems.as_ptr(),
        _ => core::ptr::null(),
    }
}

/// Returns the tzid of feature `i` as (ptr, len) together with
/// `utz_enc_tzid_len()`, for labelling problem records.
#[no_mangle]
pub extern "C" fn utz_enc_tzid_ptr(i: u32) -> *const u8 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(state) => state
            .feats
            .get(i as usize)
            .and_then(|feat| feat.tzid.as_deref())
            .map_or(core::ptr::null(), str::as_ptr),
        None => core::ptr::null(),
    }
}
#[no_mangle]
pub extern "C" fn utz_enc_tzid_len(i: u32) -> u32 {
    match unsafe { &*core::ptr::addr_of!(STATE) } {
        Some(state) => state
            .feats
            .get(i as usize)
            .and_then(|feat| feat.tzid.as_deref())
            .map_or(0, |tzid| len_u32(tzid.len())),
        None => 0,
    }
}

/// Runs stage 2 of the live encode: it compresses the resident payload
/// with one codec byte (1 gzip/zlib, 3 brotli, 4 xz; zstd is feature-gated
/// off in the wasm build) and returns the byte size a shipped `.utz` of
/// that codec would weigh: the prologue and the plaintext payload header
/// plus the compressed sections, exactly as `encode::finish()` lays them
/// out. Returns 0 on error, on an unsupported codec, or when there is no
/// payload.
#[no_mangle]
pub extern "C" fn utz_enc_compress(codec: u32) -> u32 {
    let Some(state) = (unsafe { &*core::ptr::addr_of!(STATE) }) else {
        return 0;
    };
    if state.payload.is_empty() {
        return 0;
    }
    // the codec knob sends Codec header bytes; zstd (2) is feature-gated
    // off in the wasm build, and Uncompressed needs no compress call
    let Some(codec @ (Codec::Gzip | Codec::Brotli | Codec::Xz)) =
        u8::try_from(codec).ok().and_then(Codec::from_byte)
    else {
        return 0;
    };
    let sections = &state.payload[encode::PAYLOAD_HEADER_LEN..];
    encode::compress(sections, codec).map_or(0, |compressed| {
        len_u32(encode::PROLOGUE_LEN + encode::PAYLOAD_HEADER_LEN + compressed.len())
    })
}

/// The simplify worker's misassignment state holds the last arc's
/// deviations plus the accumulators that run across arcs (wasm32 is
/// single-threaded, and the same `static mut` convention as `STATE`
/// applies).
struct WsState {
    deviations: Vec<f32>,
    simplify_acc: Acc,
    quant_acc: Acc,
}

static mut WS: WsState = WsState {
    deviations: Vec::new(),
    simplify_acc: Acc {
        area: 0.0,
        people: 0.0,
    },
    quant_acc: Acc {
        area: 0.0,
        people: 0.0,
    },
};

/// Zeroes the four running misassignment accumulators at the start of a
/// run.
#[no_mangle]
pub extern "C" fn utz_ws_reset() {
    let ws = unsafe { &mut *core::ptr::addr_of_mut!(WS) };
    ws.simplify_acc = Acc::default();
    ws.quant_acc = Acc::default();
    ws.deviations.clear();
}

/// Runs one arc through the whole simplify-worker pipeline
/// ([`misassign::arc_misassign()`]): it pre-snaps when `pre` != 0 (the
/// Q→S order), simplifies (the `algorithm` ids and `param` are as in
/// `utz_simplify`, density-weighted when `densities` is non-null and
/// `w_min` < 1), and prices the pockets and the display snap into the
/// running accumulators. The `quant` codes are the viewer's quant-knob
/// indices (0 f64, 1 f32, 2 i32, 3 i24, 4 i16). Returns the kept count:
/// the simplified coords overwrite the first `kept * 2` doubles of `xy`
/// (already display-snapped in Q→S order, raw copies otherwise), and the
/// per-kept-vertex deviations are readable via [`utz_ws_devs_ptr()`].
///
/// # Safety
/// `xy` must point at `n_pts * 2` valid doubles and `densities` at `n_pts`
/// valid doubles or be null (e.g. from `utz_alloc()`).
#[no_mangle]
pub unsafe extern "C" fn utz_ws_arc(
    algorithm: u32,
    param: f64,
    w_min: f64,
    quant: u32,
    pre: u32,
    xy: *mut f64,
    n_pts: usize,
    densities: *const f64,
) -> usize {
    let ws = &mut *core::ptr::addr_of_mut!(WS);
    let buffer = core::slice::from_raw_parts_mut(xy, n_pts * 2);
    let arc: Vec<(f64, f64)> = buffer
        .chunks_exact(2)
        .map(|chunk| (chunk[0], chunk[1]))
        .collect();
    let densities = (!densities.is_null()).then(|| core::slice::from_raw_parts(densities, n_pts));
    let params = ArcParams {
        algorithm: Simplify::from_code(algorithm, param),
        density_weight_floor: w_min,
        quant: Quant::from_code(quant),
        pre: pre != 0,
    };
    let result = misassign::arc_misassign(
        &arc,
        densities,
        &params,
        &mut ws.simplify_acc,
        &mut ws.quant_acc,
    );
    for (i, (x, y)) in result.kept.iter().enumerate() {
        buffer[i * 2] = *x;
        buffer[i * 2 + 1] = *y;
    }
    ws.deviations = result.deviations;
    result.kept.len()
}

/// Returns a pointer to the last [`utz_ws_arc()`] call's deviations (one
/// f32 per kept vertex; null when there are none). `devs[v]` prices the
/// output segment `v-1`→`v`, and it is 0 at the arc start and wherever
/// nothing was dropped.
#[no_mangle]
pub extern "C" fn utz_ws_devs_ptr() -> *const f32 {
    let ws = unsafe { &*core::ptr::addr_of!(WS) };
    if ws.deviations.is_empty() {
        core::ptr::null()
    } else {
        ws.deviations.as_ptr()
    }
}

/// Returns one running misassignment accumulator (0 for an unknown
/// index): 0 is the simplification area (km²), 1 the simplification
/// people, 2 the display-snap area (km²), and 3 the display-snap people.
#[no_mangle]
pub extern "C" fn utz_ws_stat(i: u32) -> f64 {
    let ws = unsafe { &*core::ptr::addr_of!(WS) };
    match i {
        0 => ws.simplify_acc.area,
        1 => ws.simplify_acc.people,
        2 => ws.quant_acc.area,
        3 => ws.quant_acc.people,
        _ => 0.0,
    }
}
