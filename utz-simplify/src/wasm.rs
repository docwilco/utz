//! Raw `extern "C"` surface for the tuning-viewer HTML (wasm32 only).
//!
//! No wasm-bindgen: the viz HTML loads the module with a few lines of hand
//! written glue, so the artifact stays a single self-contained file and the
//! browser preview runs byte-for-byte the same algorithms as the builder.
//!
//! JS usage sketch:
//! ```js
//! const { instance } = await WebAssembly.instantiate(wasmBytes);
//! const { memory, utz_alloc, utz_free, utz_simplify } = instance.exports;
//! const n = pts.length;                       // pts: [[x,y], ...]
//! const ptr = utz_alloc(n * 2);
//! new Float64Array(memory.buffer, ptr, n * 2).set(pts.flat());
//! const kept = utz_simplify(ALGO_RDP, ptr, n, epsDeg); // simplifies in place
//! const out = new Float64Array(memory.buffer, ptr, kept * 2).slice();
//! utz_free(ptr, n * 2);
//! ```

use crate::{simplify, Simplify};

pub const ALGO_RDP: u32 = 0;
pub const ALGO_VISVALINGAM: u32 = 1;
pub const ALGO_IMAI_IRI: u32 = 2;

/// Allocate space for `n_f64` doubles; pair every call with [`utz_free`].
#[no_mangle]
pub extern "C" fn utz_alloc(n_f64: usize) -> *mut f64 {
    let mut v = Vec::<f64>::with_capacity(n_f64);
    let ptr = v.as_mut_ptr();
    core::mem::forget(v);
    ptr
}

/// Release a buffer from [`utz_alloc`] (same `n_f64`).
///
/// # Safety
/// `ptr`/`n_f64` must come from a single prior `utz_alloc(n_f64)` call.
#[no_mangle]
pub unsafe extern "C" fn utz_free(ptr: *mut f64, n_f64: usize) {
    drop(Vec::from_raw_parts(ptr, 0, n_f64));
}

/// Simplify `n_pts` interleaved `x,y` doubles IN PLACE; returns the number of
/// points kept (the buffer's first `kept * 2` doubles). Unknown `algo` or a
/// non-positive parameter leaves the polyline unchanged.
///
/// # Safety
/// `xy` must point at `n_pts * 2` valid doubles (e.g. from [`utz_alloc`]).
#[no_mangle]
pub unsafe extern "C" fn utz_simplify(algo: u32, xy: *mut f64, n_pts: usize, param: f64) -> usize {
    let buf = core::slice::from_raw_parts_mut(xy, n_pts * 2);
    let pts: Vec<(f64, f64)> = buf.chunks_exact(2).map(|c| (c[0], c[1])).collect();
    let out = simplify(
        match algo {
            ALGO_RDP => Simplify::Rdp { eps: param },
            ALGO_VISVALINGAM => Simplify::Visvalingam { min_area: param },
            ALGO_IMAI_IRI => Simplify::ImaiIri { eps: param },
            _ => Simplify::None,
        },
        &pts,
    );
    for (i, (x, y)) in out.iter().enumerate() {
        buf[i * 2] = *x;
        buf[i * 2 + 1] = *y;
    }
    out.len()
}
