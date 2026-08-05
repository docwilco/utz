//! The raw `extern "C"` surface for the tuning-viewer HTML (wasm32 only).
//!
//! There is no wasm-bindgen involved: the viz HTML loads the module with a
//! few lines of hand-written glue, so the artifact stays a single
//! self-contained file and the browser preview runs byte-for-byte the same
//! algorithms as the builder.
//!
//! The JS side uses the module like this:
//! ```js
//! const { instance } = await WebAssembly.instantiate(wasmBytes);
//! const { memory, utz_alloc, utz_free, utz_simplify } = instance.exports;
//! const n = points.length;                       // points: [[x,y], ...]
//! const ptr = utz_alloc(n * 2);
//! new Float64Array(memory.buffer, ptr, n * 2).set(points.flat());
//! const kept = utz_simplify(ALGORITHM_RDP, ptr, n, epsilonDeg); // simplifies in place
//! const out = new Float64Array(memory.buffer, ptr, kept * 2).slice();
//! utz_free(ptr, n * 2);
//! ```

use crate::{simplify, simplify_weighted, DensityWeight, Simplify};

/// Allocates space for `n_f64` doubles; every call must be paired with
/// [`utz_free()`].
#[no_mangle]
pub extern "C" fn utz_alloc(n_f64: usize) -> *mut f64 {
    let mut buffer = Vec::<f64>::with_capacity(n_f64);
    let ptr = buffer.as_mut_ptr();
    core::mem::forget(buffer);
    ptr
}

/// Releases a buffer from [`utz_alloc()`] (with the same `n_f64`).
///
/// # Safety
/// `ptr`/`n_f64` must come from a single prior `utz_alloc(n_f64)` call.
#[no_mangle]
pub unsafe extern "C" fn utz_free(ptr: *mut f64, n_f64: usize) {
    drop(Vec::from_raw_parts(ptr, 0, n_f64));
}

/// Simplifies `n_points` interleaved `x,y` doubles IN PLACE and returns the
/// number of points kept (the buffer's first `kept * 2` doubles). An unknown
/// `algorithm` or a non-positive parameter leaves the polyline unchanged.
///
/// # Safety
/// `xy` must point at `n_points * 2` valid doubles (e.g. from [`utz_alloc()`]).
#[no_mangle]
pub unsafe extern "C" fn utz_simplify(
    algorithm: u32,
    xy: *mut f64,
    n_points: usize,
    param: f64,
) -> usize {
    let buffer = core::slice::from_raw_parts_mut(xy, n_points * 2);
    let points: Vec<(f64, f64)> = buffer
        .chunks_exact(2)
        .map(|chunk| (chunk[0], chunk[1]))
        .collect();
    let algorithm = Simplify::from_code(algorithm, param);
    let out = simplify(algorithm, &points);
    for (i, (x, y)) in out.iter().enumerate() {
        buffer[i * 2] = *x;
        buffer[i * 2 + 1] = *y;
    }
    out.len()
}

/// [`utz_simplify()`] with population-density weighting: `densities` points
/// at `n_points` per-vertex densities (people/km²), mapped through
/// [`DensityWeight::new()`]`(w_min)`, so the browser's weighting slider runs
/// the exact map the builder uses, not a JS reimplementation. `w_min ≥ 1`
/// turns weighting off (identical to [`utz_simplify()`]).
///
/// # Safety
/// `xy` must point at `n_points * 2` valid doubles and `densities` at `n_points`
/// valid doubles (e.g. from [`utz_alloc()`]).
#[no_mangle]
pub unsafe extern "C" fn utz_simplify_w(
    algorithm: u32,
    xy: *mut f64,
    n_points: usize,
    param: f64,
    densities: *const f64,
    w_min: f64,
) -> usize {
    let buffer = core::slice::from_raw_parts_mut(xy, n_points * 2);
    let points: Vec<(f64, f64)> = buffer
        .chunks_exact(2)
        .map(|chunk| (chunk[0], chunk[1]))
        .collect();
    let model = DensityWeight::new(w_min);
    let weights: Vec<f64> = core::slice::from_raw_parts(densities, n_points)
        .iter()
        .map(|&density| model.weight(density))
        .collect();
    let algorithm = Simplify::from_code(algorithm, param);
    let out = simplify_weighted(algorithm, &points, &weights);
    for (i, (x, y)) in out.iter().enumerate() {
        buffer[i * 2] = *x;
        buffer[i * 2 + 1] = *y;
    }
    out.len()
}
