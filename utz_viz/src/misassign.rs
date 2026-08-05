//! Misassigned-area/population pricing of a simplified arc versus its raw
//! (pre-quantization) form, the math behind the viewer's "misassigned"
//! stats (previously a JS reimplementation inside the simplify worker).
//!
//! Every simplifier in the menu keeps a subset of the input vertices, so
//! each output segment covers a contiguous run of raw vertices and the
//! misassigned region decomposes exactly into "pockets" between the raw
//! sub-chain and its shortcut (split where the chain crosses the shortcut
//! line). [`arc_misassign()`] runs the whole per-arc worker pipeline: an
//! optional pre-snap (the Q→S order), simplification via `utz_simplify`, a
//! walk-match of kept vertices back to input indices, pocket pricing
//! ([`pockets()`]), per-segment deviations ([`dev_max()`]), and
//! display-snap pricing ([`quant_quad()`]).
//!
//! Float semantics follow the original JS exactly (f64 throughout, JS
//! `Math.round`/`Math.fround` rounding in [`qc()`]); deviations narrow to
//! f32 like the worker's `Float32Array`.

use utz_simplify::{simplify, simplify_weighted, DensityWeight, Simplify};

/// The display quantization mode, the viewer's quant knob (`f64` is "no
/// snap").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quant {
    /// Coordinates pass through unchanged (no snap).
    F64,
    /// Rounds to the nearest f32.
    F32,
    /// Rounds to 1e-7 degrees (the common "integer degrees ×10^7" lattice).
    I32,
    /// Snaps to the signed 24-bit lattice over the axis span.
    I24,
    /// Snaps to the signed 16-bit lattice over the axis span.
    I16,
}

impl Quant {
    /// The mode for a viewer quant-knob index (0 f64, 1 f32, 2 i32, 3 i24,
    /// 4 i16); unknown codes read as [`Quant::F64`] (no snap).
    #[must_use]
    pub const fn from_code(code: u32) -> Self {
        match code {
            1 => Self::F32,
            2 => Self::I32,
            3 => Self::I24,
            4 => Self::I16,
            _ => Self::F64,
        }
    }
}

/// JS `Math.round`: the nearest integer, exact halves toward positive
/// infinity (Rust's `round` breaks ties away from zero instead).
fn js_round(value: f64) -> f64 {
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// One coordinate after the display snap: `span` is the axis half-range
/// (180 for longitude, 90 for latitude). Port of the viewer's `qc`.
#[must_use]
pub fn qc(value: f64, mode: Quant, span: f64) -> f64 {
    match mode {
        Quant::F64 => value,
        #[expect(
            clippy::cast_possible_truncation,
            reason = "JS Math.fround semantics: round the f64 to the nearest f32 and widen back"
        )]
        Quant::F32 => f64::from(value as f32),
        Quant::I32 => js_round(value * 1e7) / 1e7,
        Quant::I24 => snap(value, span, utz_encode::qmax_for(24)),
        Quant::I16 => snap(value, span, utz_encode::qmax_for(16)),
    }
}

/// The integer-grid snap round trip with JS rounding: quantize onto the
/// half-range `qmax` grid, dequantize back to degrees.
fn snap(value: f64, span: f64, qmax: f64) -> f64 {
    js_round(value / span * qmax) / qmax * span
}

/// Running misassignment totals: area in km² and people.
#[derive(Clone, Copy, Debug, Default)]
pub struct Acc {
    pub area: f64,
    pub people: f64,
}

/// One pocket between a raw sub-chain and its shortcut, as running sums so
/// each pricing rule divides in its own float order: the viewer prices
/// people from `dens_sum / count`, the accuracy CLI samples its density
/// grid at (`lon_sum / count`, `lat_sum / count`).
#[derive(Clone, Copy, Debug)]
pub struct Pocket {
    /// The signed anchored-shoelace area of the pocket (deg²).
    pub area: f64,
    /// The sum of the pocket's chain-vertex longitudes (crossing points
    /// count as chain vertices).
    pub lon_sum: f64,
    /// The sum of the pocket's chain-vertex latitudes.
    pub lat_sum: f64,
    /// The sum of the per-vertex densities behind the chain vertices
    /// (0 when no densities were given).
    pub dens_sum: f64,
    /// The number of chain vertices behind the sums.
    pub count: f64,
}

/// Decompose the region between `chain` and its shortcut
/// (`chain[0]` → `chain[last]`) into pockets, splitting the anchored
/// shoelace accumulation wherever the chain crosses the shortcut line, and
/// hand each pocket to `flush`. `densities` is one density per chain vertex.
///
/// # Panics
///
/// Panics if `chain` is empty or `densities` is shorter than `chain`.
pub fn pocket_scan(
    chain: &[(f64, f64)],
    densities: Option<&[f64]>,
    mut flush: impl FnMut(&Pocket),
) {
    let density = |i: usize| densities.map_or(0.0, |values| values[i]);
    let (ax, ay) = chain[0];
    let (bx, by) = *chain.last().unwrap();
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let mut pocket = Pocket {
        area: 0.0,
        lon_sum: ax,
        lat_sum: ay,
        dens_sum: density(0),
        count: 1.0,
    };
    // previous vertex and its side of the shortcut line (0 for the start:
    // it sits on the line by construction)
    let (mut px, mut py, mut previous_side) = (ax, ay, 0.0);
    for k in 0..chain.len() - 1 {
        let (qx, qy) = chain[k + 1];
        let current_side = dx * (qy - ay) - dy * (qx - ax);
        if len2 > 0.0 && previous_side * current_side < 0.0 {
            // chain crosses the shortcut line: split the step at the crossing
            let t = previous_side / (previous_side - current_side);
            let crossing_x = px + t * (qx - px);
            let crossing_y = py + t * (qy - py);
            pocket.area += ((px - ax) * (crossing_y - ay) - (py - ay) * (crossing_x - ax)) / 2.0;
            flush(&pocket);
            pocket = Pocket {
                area: ((crossing_x - ax) * (qy - ay) - (crossing_y - ay) * (qx - ax)) / 2.0,
                lon_sum: crossing_x + qx,
                lat_sum: crossing_y + qy,
                dens_sum: density(k) + density(k + 1),
                count: 2.0,
            };
        } else {
            pocket.area += ((px - ax) * (qy - ay) - (py - ay) * (qx - ax)) / 2.0;
            pocket.lon_sum += qx;
            pocket.lat_sum += qy;
            pocket.dens_sum += density(k + 1);
            pocket.count += 1.0;
        }
        (px, py, previous_side) = (qx, qy, current_side);
    }
    flush(&pocket);
}

/// Misassignment between the raw sub-chain `arc[i0..=i1]` and its shortcut,
/// added to `acc`: pocket area latitude-corrected to km² (111.32²·cos),
/// people priced with the mean per-vertex density over each pocket's chain
/// (the boundary densities hug exactly where pockets live). Port of the
/// worker's `pockets`; always priced on raw coordinates, so the stat stays
/// "vs raw ε=0, pre-quant" in both orders.
///
/// # Panics
///
/// Panics if `i0..=i1` is out of bounds for `arc` (or `densities`).
pub fn pockets(arc: &[(f64, f64)], densities: Option<&[f64]>, i0: usize, i1: usize, acc: &mut Acc) {
    pocket_scan(
        &arc[i0..=i1],
        densities.map(|values| &values[i0..=i1]),
        |pocket| {
            let km2 = pocket.area.abs()
                * utz_encode::KM_PER_DEG
                * utz_encode::KM_PER_DEG
                * (pocket.lat_sum / pocket.count * core::f64::consts::PI / 180.0).cos();
            acc.area += km2;
            acc.people += km2 * pocket.dens_sum / pocket.count;
        },
    );
}

/// Area (km²) and people between a pre-snap segment `a`→`b` and its drawn
/// (quantized) counterpart `qa`→`qb`, added to `acc`: the misassignment the
/// display snap adds on top of the simplification pockets. Splits at the
/// crossing when the two segments intersect (the snap usually moves the
/// ends to opposite sides). `density` is the segment's density (people/km²).
/// Port of the worker's `quantQuad`.
pub fn quant_quad(
    a: (f64, f64),
    b: (f64, f64),
    qa: (f64, f64),
    qb: (f64, f64),
    density: f64,
    acc: &mut Acc,
) {
    let triangle_area = |x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64| {
        ((x2 - x1) * (y3 - y1) - (y2 - y1) * (x3 - x1)).abs() / 2.0
    };
    let (rx, ry) = (b.0 - a.0, b.1 - a.1);
    let (sx, sy) = (qb.0 - qa.0, qb.1 - qa.1);
    let denominator = rx * sy - ry * sx;
    let (t, u) = if denominator == 0.0 {
        (-1.0, -1.0)
    } else {
        (
            ((qa.0 - a.0) * sy - (qa.1 - a.1) * sx) / denominator,
            ((qa.0 - a.0) * ry - (qa.1 - a.1) * rx) / denominator,
        )
    };
    let area = if t > 0.0 && t < 1.0 && u > 0.0 && u < 1.0 {
        let crossing_x = a.0 + t * rx;
        let crossing_y = a.1 + t * ry;
        triangle_area(a.0, a.1, crossing_x, crossing_y, qa.0, qa.1)
            + triangle_area(b.0, b.1, crossing_x, crossing_y, qb.0, qb.1)
    } else {
        (a.0 * (b.1 - qa.1) + b.0 * (qb.1 - a.1) + qb.0 * (qa.1 - b.1) + qa.0 * (a.1 - qb.1)).abs()
            / 2.0
    };
    let km2 = area
        * utz_encode::KM_PER_DEG
        * utz_encode::KM_PER_DEG
        * (f64::midpoint(a.1, b.1) * core::f64::consts::PI / 180.0).cos();
    acc.area += km2;
    acc.people += km2 * density;
}

/// Max perpendicular deviation (meters) of the raw vertices strictly
/// between `i0` and `i1` from the raw chord `i0`→`i1`, on a locally
/// cos-corrected flat map (·111 320 m/deg): the same "vs raw ε=0,
/// pre-quant" reference as the misassigned stat, so divergence highlighting
/// shows where the simplifier spent its error budget. Port of the worker's
/// `devMax`.
///
/// # Panics
///
/// Panics if `i0..=i1` is out of bounds for `arc`.
#[must_use]
pub fn dev_max(arc: &[(f64, f64)], i0: usize, i1: usize) -> f64 {
    let cos_correction =
        (f64::midpoint(arc[i0].1, arc[i1].1) * core::f64::consts::PI / 180.0).cos();
    let (ax, ay) = (arc[i0].0 * cos_correction, arc[i0].1);
    let (bx, by) = (arc[i1].0 * cos_correction, arc[i1].1);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let mut max_d2 = 0.0f64;
    for &(x, y) in &arc[i0 + 1..i1] {
        let (px, py) = (x * cos_correction, y);
        let t = if len2 == 0.0 {
            0.0
        } else {
            ((px - ax) * dx + (py - ay) * dy) / len2
        }
        .clamp(0.0, 1.0);
        let ex = px - (ax + t * dx);
        let ey = py - (ay + t * dy);
        let d2 = ex * ex + ey * ey;
        if d2 > max_d2 {
            max_d2 = d2;
        }
    }
    max_d2.sqrt() * utz_encode::METERS_PER_DEG
}

/// The knobs of one worker run, applied to every arc.
#[derive(Clone, Copy, Debug)]
pub struct ArcParams {
    /// The algorithm and parameter handed to `utz_simplify` (the
    /// parameter is in degrees; Visvalingam takes degrees squared).
    pub algorithm: Simplify,
    /// The density weighting floor; `>= 1` (or no densities) turns
    /// weighting off.
    pub density_weight_floor: f64,
    /// The display quantization mode.
    pub quant: Quant,
    /// Q→S order: snap to the display lattice BEFORE simplifying (ignored
    /// for [`Quant::F64`], where there is no lattice)
    pub pre: bool,
}

/// One arc's trip through [`arc_misassign`]: the simplified coordinates and
/// one deviation per kept vertex (`deviations[v]` prices the output segment
/// `v-1`→`v`; 0 at the arc start and wherever nothing was dropped).
#[derive(Clone, Debug)]
pub struct ArcResult {
    pub kept: Vec<(f64, f64)>,
    pub deviations: Vec<f32>,
}

/// One arc through the whole worker pipeline: pre-snap (Q→S) or post-snap
/// display pricing (S→Q), simplify (density-weighted when `densities` is
/// given and `w_min < 1`), exact walk-match of kept vertices back to input
/// indices, then pocket pricing into `simplify_acc` ([`pockets`],
/// [`dev_max`]) and display-snap pricing into `quant_acc`
/// ([`quant_quad`]). `densities` is one density (people/km²) per arc vertex.
///
/// # Panics
///
/// Panics if `densities` is shorter than `arc`, or if the simplifier returns
/// a vertex that is not a bit-exact copy of an input vertex (every algorithm
/// in the menu keeps a subset, so the walk-match is exact by invariant).
#[must_use]
pub fn arc_misassign(
    arc: &[(f64, f64)],
    densities: Option<&[f64]>,
    params: &ArcParams,
    simplify_acc: &mut Acc,
    quant_acc: &mut Acc,
) -> ArcResult {
    let n = arc.len();
    let pre = params.pre && params.quant != Quant::F64;
    let snapped: Vec<(f64, f64)>;
    // what the simplifier eats: the walk-match below compares against this,
    // not the raw arc
    let source: &[(f64, f64)] = if pre {
        snapped = arc
            .iter()
            .map(|&(x, y)| (qc(x, params.quant, 180.0), qc(y, params.quant, 90.0)))
            .collect();
        &snapped
    } else {
        arc
    };
    let kept = match densities {
        Some(values) if params.density_weight_floor < 1.0 => {
            let model = DensityWeight::new(params.density_weight_floor);
            let weights: Vec<f64> = values
                .iter()
                .map(|&density| model.weight(density))
                .collect();
            simplify_weighted(params.algorithm, source, &weights)
        }
        _ => simplify(params.algorithm, source),
    };
    let kept_len = kept.len();
    let mut deviations = vec![0.0f32; kept_len];
    let mut raw_indices: Option<Vec<usize>> = None;
    if kept_len < n {
        // walk-match kept verts to input indices (exact equality against
        // `source`); snapping is 1:1 per vertex, so those indices ARE raw
        // indices and pockets price the raw chain
        let mut indices = Vec::with_capacity(kept_len);
        let mut j = 0usize;
        let mut previous: Option<usize> = None;
        for (vertex, &point) in kept.iter().enumerate() {
            while source[j] != point {
                j += 1;
            }
            indices.push(j);
            if let Some(previous) = previous {
                if j > previous + 1 {
                    pockets(arc, densities, previous, j, simplify_acc);
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "the worker stores deviations in a Float32Array; narrowing to f32 is the ABI"
                    )]
                    {
                        deviations[vertex] = dev_max(arc, previous, j) as f32;
                    }
                }
            }
            previous = Some(j);
            j += 1;
        }
        raw_indices = Some(indices);
    }
    if params.quant != Quant::F64 {
        // price the display snap per drawn segment, on top of the pockets
        // (which deliberately exclude quantization)
        for vertex in 1..kept_len {
            let (index_a, index_b) = raw_indices
                .as_ref()
                .map_or((vertex - 1, vertex), |indices| {
                    (indices[vertex - 1], indices[vertex])
                });
            let (a, b, qa, qb) = if pre {
                // Q→S: drawn = output (already snapped); pre-snap = raw
                (arc[index_a], arc[index_b], kept[vertex - 1], kept[vertex])
            } else {
                // S→Q: output is raw copies; drawn = display snap of it
                let (a, b) = (kept[vertex - 1], kept[vertex]);
                (
                    a,
                    b,
                    (qc(a.0, params.quant, 180.0), qc(a.1, params.quant, 90.0)),
                    (qc(b.0, params.quant, 180.0), qc(b.1, params.quant, 90.0)),
                )
            };
            let segment_density = densities.map_or(0.0, |values| {
                f64::midpoint(values[index_a], values[index_b])
            });
            quant_quad(a, b, qa, qb, segment_density, quant_acc);
        }
    }
    ArcResult { kept, deviations }
}

#[cfg(test)]
mod tests {
    //! Agreement with the original JS helpers: the expected values come
    //! from running the worker's `qc`/`pockets`/`quantQuad`/`devMax` (and a
    //! replica of the worker loop body around a line-by-line JS port of
    //! `utz_simplify`'s rdp) verbatim under node on the same inputs.

    use super::*;

    const ARC_A: [(f64, f64); 6] = [
        (0.0, 50.0),
        (0.1, 50.2),
        (0.2, 50.05),
        (0.3, 50.3),
        (0.4, 50.0),
        (0.5, 50.1),
    ];
    /// exactly f32-representable, like the viewer's `Float32Array` densities
    const DENS_A: [f64; 6] = [120.0, 300.5, 80.0, 500.0, 60.25, 240.0];
    const ARC_B: [(f64, f64); 6] = [
        (0.0, 10.0),
        (0.1, 10.05),
        (0.2, 9.95),
        (0.3, 10.06),
        (0.4, 9.94),
        (0.5, 10.0),
    ];

    fn close(got: f64, want: f64) {
        assert!(
            (got - want).abs() <= want.abs() * 1e-9 + 1e-12,
            "got {got}, want {want}"
        );
    }

    #[test]
    fn js_round_halves_toward_positive_infinity() {
        assert!((js_round(2.5) - 3.0).abs() < f64::EPSILON);
        assert!((js_round(-4.5) - -4.0).abs() < f64::EPSILON);
        assert!(js_round(-0.5).abs() < f64::EPSILON);
        assert!((js_round(-4.7) - -5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn qc_matches_js() {
        let values = [
            123.456_789_123,
            -89.999_987_65,
            0.300_000_000_000_000_04,
            -0.15,
            179.99999,
            33.333_333_333_333_336,
        ];
        let modes = [Quant::F64, Quant::F32, Quant::I32, Quant::I24, Quant::I16];
        // want[value][mode] = [qc(v, mode, 180), qc(v, mode, 90)]
        let want: [[[f64; 2]; 5]; 6] = [
            [
                [123.456_789_123, 123.456_789_123],
                [123.456_787_109_375, 123.456_787_109_375],
                [123.456_789_1, 123.456_789_1],
                [123.456_795_627_688_85, 123.456_784_898_851_51],
                [123.457_136_753_440_98, 123.457_136_753_440_98],
            ],
            [
                [-89.999_987_65, -89.999_987_65],
                [-89.999_984_741_210_94, -89.999_984_741_210_94],
                [-89.999_987_6, -89.999_987_6],
                [-89.999_989_271_162_66, -89.999_989_271_162_66],
                [-89.997_253_334_147_16, -90.0],
            ],
            [
                [0.300_000_000_000_000_04, 0.300_000_000_000_000_04],
                [0.300_000_011_920_928_96, 0.300_000_011_920_928_96],
                [0.3, 0.3],
                [0.299_999_749_660_462_1, 0.299_999_749_660_462_1],
                [0.302_133_243_812_372_2, 0.299_386_577_959_532_44],
            ],
            [
                [-0.15, -0.15],
                [-0.150_000_005_960_464_48, -0.150_000_005_960_464_48],
                [-0.15, -0.15],
                [-0.150_010_603_667_569_6, -0.149_999_874_830_231_06],
                [-0.148_319_956_053_346_36, -0.151_066_621_906_186_1],
            ],
            [
                [179.99999, 179.99999],
                [179.999_984_741_210_94, 179.999_984_741_210_94],
                [179.99999, 179.99999],
                [180.0, 179.999_989_271_162_66],
                [180.0, 180.0],
            ],
            [
                [33.333_333_333_333_336, 33.333_333_333_333_336],
                [33.333_332_061_767_58, 33.333_332_061_767_58],
                [33.333_333_3, 33.333_333_3],
                [33.333_338_896_434_18, 33.333_328_167_596_84],
                [33.333_536_790_063_18, 33.333_536_790_063_18],
            ],
        ];
        for (value, rows) in values.iter().zip(&want) {
            for (mode, pair) in modes.iter().zip(rows) {
                close(qc(*value, *mode, 180.0), pair[0]);
                close(qc(*value, *mode, 90.0), pair[1]);
            }
        }
    }

    #[test]
    fn pockets_matches_js() {
        let mut acc = Acc::default();
        pockets(&ARC_A, Some(&DENS_A), 0, 5, &mut acc);
        close(acc.area, 357.487_684_173_062_4);
        close(acc.people, 90_077.936_754_805);

        let mut acc = Acc::default();
        pockets(&ARC_A, None, 0, 5, &mut acc);
        close(acc.area, 357.487_684_173_062_4);
        close(acc.people, 0.0);

        // ARC_B zigzags across its chord: exercises the crossing splits
        let mut acc = Acc::default();
        pockets(&ARC_B, None, 0, 5, &mut acc);
        close(acc.area, 168.079_232_425_042_62);
        close(acc.people, 0.0);
    }

    #[test]
    fn dev_max_matches_js() {
        close(dev_max(&ARC_A, 0, 5), 25_508.128_009_511_387);
        close(dev_max(&ARC_B, 1, 4), 8_693.657_086_519_403);
    }

    #[test]
    fn quant_quad_matches_js() {
        let mut acc = Acc::default();
        quant_quad(
            (10.0, 45.0),
            (10.3, 45.2),
            (10.001, 45.002),
            (10.299, 45.198),
            150.0,
            &mut acc,
        );
        close(acc.area, 1.749_452_203_995_284_4);
        close(acc.people, 262.417_830_599_292_64);

        // qa/qb on opposite sides of a→b: the segments cross
        let mut acc = Acc::default();
        quant_quad(
            (0.0, 0.001),
            (0.01, -0.001),
            (0.001, -0.002),
            (0.009, 0.002),
            42.0,
            &mut acc,
        );
        close(acc.area, 0.173_489_993_6);
        close(acc.people, 7.286_579_731_2);
    }

    fn check_result(result: &ArcResult, coords: &[f64], deviations: &[f64]) {
        assert_eq!(result.kept.len() * 2, coords.len());
        for (got, want) in result.kept.iter().zip(coords.chunks_exact(2)) {
            close(got.0, want[0]);
            close(got.1, want[1]);
        }
        assert_eq!(result.deviations.len(), deviations.len());
        for (got, want) in result.deviations.iter().zip(deviations) {
            close(f64::from(*got), *want);
        }
    }

    #[test]
    fn driver_sq_i16_matches_js() {
        let params = ArcParams {
            algorithm: Simplify::Rdp { epsilon: 0.15 },
            density_weight_floor: 1.0,
            quant: Quant::I16,
            pre: false,
        };
        let (mut simplify_acc, mut quant_acc) = (Acc::default(), Acc::default());
        let result = arc_misassign(
            &ARC_A,
            Some(&DENS_A),
            &params,
            &mut simplify_acc,
            &mut quant_acc,
        );
        check_result(
            &result,
            &[0.0, 50.0, 0.3, 50.3, 0.5, 50.1],
            &[0.0, 9_008.896_484_375, 13_217.300_781_25],
        );
        close(simplify_acc.area, 309.771_666_644_827_86);
        close(simplify_acc.people, 82_036.021_587_182_68);
        close(quant_acc.area, 3.832_818_936_460_496_3);
        close(quant_acc.people, 1_260.744_989_596_838_9);
    }

    #[test]
    fn driver_qs_i24_matches_js() {
        let params = ArcParams {
            algorithm: Simplify::Rdp { epsilon: 0.05 },
            density_weight_floor: 1.0,
            quant: Quant::I24,
            pre: true,
        };
        let (mut simplify_acc, mut quant_acc) = (Acc::default(), Acc::default());
        let result = arc_misassign(&ARC_B, None, &params, &mut simplify_acc, &mut quant_acc);
        check_result(
            &result,
            &[
                0.0,
                9.999_995_231_627_85,
                0.099_992_763_995_261_68,
                10.050_002_342_462_818,
                0.200_006_985_665_200_45,
                9.949_998_849_630_218,
                0.299_999_749_660_462_1,
                10.060_001_618_862_344,
                0.399_992_513_655_723_8,
                9.939_999_573_230_692,
                0.500_006_735_325_662_5,
                9.999_995_231_627_85,
            ],
            &[0.0; 6],
        );
        close(simplify_acc.area, 0.0);
        close(simplify_acc.people, 0.0);
        close(quant_acc.area, 0.020_852_539_660_342_312);
        close(quant_acc.people, 0.0);
    }

    #[test]
    fn driver_qs_i16_drop_matches_js() {
        // Q→S with drops: the walk-match runs against the snapped source
        let params = ArcParams {
            algorithm: Simplify::Rdp { epsilon: 0.15 },
            density_weight_floor: 1.0,
            quant: Quant::I16,
            pre: true,
        };
        let (mut simplify_acc, mut quant_acc) = (Acc::default(), Acc::default());
        let result = arc_misassign(&ARC_B, None, &params, &mut simplify_acc, &mut quant_acc);
        check_result(
            &result,
            &[
                0.0,
                10.000_610_370_189_52,
                0.499_893_185_216_834,
                10.000_610_370_189_52,
            ],
            &[0.0, 6_679.200_195_312_5],
        );
        close(simplify_acc.area, 168.079_232_425_042_62);
        close(simplify_acc.people, 0.0);
        close(quant_acc.area, 3.724_043_811_572_012_3);
        close(quant_acc.people, 0.0);
    }

    #[test]
    fn driver_none_f32_matches_js() {
        // algorithm None: no pockets, pure display-snap pricing
        let params = ArcParams {
            algorithm: Simplify::None,
            density_weight_floor: 1.0,
            quant: Quant::F32,
            pre: false,
        };
        let (mut simplify_acc, mut quant_acc) = (Acc::default(), Acc::default());
        let result = arc_misassign(
            &ARC_A,
            Some(&DENS_A),
            &params,
            &mut simplify_acc,
            &mut quant_acc,
        );
        assert_eq!(result.kept.len(), 6);
        assert_eq!(result.kept, ARC_A.to_vec());
        close(simplify_acc.area, 0.0);
        close(simplify_acc.people, 0.0);
        close(quant_acc.area, 0.002_116_376_655_329_338);
        close(quant_acc.people, 0.471_441_312_503_589_45);
    }

    #[test]
    fn driver_accumulates_across_arcs() {
        let params = ArcParams {
            algorithm: Simplify::Rdp { epsilon: 0.15 },
            density_weight_floor: 1.0,
            quant: Quant::I16,
            pre: false,
        };
        let (mut simplify_acc, mut quant_acc) = (Acc::default(), Acc::default());
        let _ = arc_misassign(
            &ARC_A,
            Some(&DENS_A),
            &params,
            &mut simplify_acc,
            &mut quant_acc,
        );
        let _ = arc_misassign(&ARC_B, None, &params, &mut simplify_acc, &mut quant_acc);
        close(simplify_acc.area, 477.850_899_069_870_5);
        close(simplify_acc.people, 82_036.021_587_182_68);
        close(quant_acc.area, 7.556_862_748_032_509);
        close(quant_acc.people, 1_260.744_989_596_838_9);
    }
}
