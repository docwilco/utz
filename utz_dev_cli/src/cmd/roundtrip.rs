//! Runs the end-to-end roundtrip: it encodes the real asset, decodes it
//! with the runtime Finder, and validates `lookup()` against a linear
//! first-hit PIP scan over the same quantized geometry (the `grid_bench`
//! reference).
//!
//! ```text
//! utz_dev_cli roundtrip [ds] [eps_m] [npts]
//! ```

use std::time::Instant;

use utz_encode::encode::{self, Codec, Params};
use utz_encode::{topo, Feat};

#[derive(clap::Args)]
pub struct Args {
    /// The dataset, one of [land-]now|1970|all.
    #[arg(default_value = "now")]
    ds: String,
    /// The simplification tolerance in meters.
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// The number of sample points.
    #[arg(default_value_t = 100_000)]
    npts: usize,
}

/// # Errors
/// The command fails on a dataset load/parse or encode failure.
///
/// # Panics
/// The command panics if the runtime `Finder` rejects the asset just
/// encoded, or if the decoded release string does not round-trip.
#[expect(
    clippy::too_many_lines,
    reason = "linear bench/report command; the stages share the run's accumulators"
)]
pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, eps_m, n_points) = (args.ds, args.eps_m, args.npts);
    let quant_bits = 24u32;

    let features = utz_build::load(&dataset)?;
    let params = Params {
        dataset: utz_build::dataset(&dataset)?.code(),
        tzbb_release: "roundtrip-dev",
        eps_m,
        quant_bits,
        grid_deg: 2.0,
        codec: Codec::Uncompressed,
        simplify: encode::SimplifyAlgo::default(),
        geom: encode::GeomEncoding::default(),
        density_weight_floor: None,
    };
    let container = encode::encode(&features, &params)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "container size ≪ 2^53; KB display"
    )]
    let kb = container.len() as f64 / 1024.0;
    println!(
        "{} container: {kb:.1} KB uncompressed",
        dataset.to_uppercase()
    );

    let finder = utz::Finder::from_reader(&container[..]).expect("decode");
    assert_eq!(finder.tzbb_release(), "roundtrip-dev");

    // reference: linear first-hit over the same quantized geometry
    #[expect(
        clippy::cast_precision_loss,
        reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64"
    )]
    let qmax = ((1u64 << (quant_bits - 1)) - 1) as f64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "|lon/180·qmax| ≤ qmax < 2^31"
    )]
    let quantize_lon = |lon: f64| (lon / 180.0 * qmax).round() as i32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "|lat/90·qmax| ≤ qmax < 2^31"
    )]
    let quantize_lat = |lat: f64| (lat / 90.0 * qmax).round() as i32;
    let topology = topo::build_topology(&features, eps_m / 111_320.0);
    let dequantized_arcs: Vec<Vec<(f64, f64)>> = topology
        .arc_coords
        .iter()
        .map(|arc| {
            let mut qcoords: Vec<(i32, i32)> = arc
                .iter()
                .map(|&(lon, lat)| (quantize_lon(lon), quantize_lat(lat)))
                .collect();
            qcoords.dedup();
            qcoords
                .iter()
                .map(|&(qlon, qlat)| {
                    (
                        f64::from(qlon) / qmax * 180.0,
                        f64::from(qlat) / qmax * 90.0,
                    )
                })
                .collect()
        })
        .collect();
    let quantized = topology.reconstruct(&features, &dequantized_arcs);
    let refs = build_refs(&quantized, qmax);

    let points = gen_pts(n_points);
    let start = Instant::now();
    let got: Vec<Option<&str>> = points
        .iter()
        .map(|&(lon, lat)| {
            finder
                .lookup(utz::Position { lon, lat })
                .expect("sample point in range")
        })
        .collect();
    let lazy_elapsed = start.elapsed();
    #[expect(
        clippy::cast_precision_loss,
        reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/point display"
    )]
    let us_per_point = |elapsed: std::time::Duration| elapsed.as_micros() as f64 / n_points as f64;

    let (mut diff, mut wrong, mut shown) = (0usize, 0usize, 0usize);
    for (index, &(lon, lat)) in points.iter().enumerate() {
        let (px, py) = (quantize_lon(lon), quantize_lat(lat));
        let want = lookup_linear(&refs, px, py);
        let finder_tz = got[index].map(std::string::ToString::to_string);
        if finder_tz == want {
            continue;
        }
        diff += 1;
        // finder answer valid if its feature actually contains the point
        let ok = finder_tz.as_deref().is_some_and(|tzid| {
            refs.iter().any(|(ref_tzid, polys)| {
                ref_tzid == tzid && polys.iter().any(|poly| contains(poly, px, py))
            })
        });
        if !ok {
            wrong += 1;
            if shown < 8 {
                shown += 1;
                println!("  WRONG ({lon:.4},{lat:.4}) finder={finder_tz:?} linear={want:?}");
            }
        }
    }
    println!(
        "disagreements: {diff} ({wrong} wrong, {} benign-overlap)",
        diff - wrong
    );
    println!(
        "finder.lookup: {:.2} µs/point over {n_points}",
        us_per_point(lazy_elapsed)
    );

    // coarse sanity: must answer everywhere with-oceans covers, cheaply
    let start = Instant::now();
    let answered = points
        .iter()
        .filter(|&&(lon, lat)| {
            finder
                .lookup_coarse(utz::Position { lon, lat })
                .expect("sample point in range")
                .is_some()
        })
        .count();
    println!(
        "lookup_coarse: {answered}/{n_points} answered, {:.2} µs/point",
        us_per_point(start.elapsed())
    );

    // zero-copy static source (core-rung path) must answer identically —
    // lazy lookup streams PIP straight off the borrowed bytes
    let static_finder = utz::Finder::from_static(Box::leak(container.clone().into_boxed_slice()))
        .expect("static decode");
    let n_static = n_points.min(20_000);
    for &(lon, lat) in points.iter().take(n_static) {
        assert_eq!(
            static_finder.lookup(utz::Position { lon, lat }),
            finder.lookup(utz::Position { lon, lat }),
            "static ({lon},{lat})"
        );
    }
    println!("from_static lookup: agrees over {n_static}");

    // eager mode: preload, must agree everywhere; report heap + speedup
    let mut eager_finder = utz::Finder::from_reader(&container[..]).expect("decode");
    let ((), heap, ms) = super::window_sweep::measure(|| eager_finder.preload());
    let start = Instant::now();
    let eager_got: Vec<Option<&str>> = points
        .iter()
        .map(|&(lon, lat)| {
            eager_finder
                .lookup(utz::Position { lon, lat })
                .expect("sample point in range")
        })
        .collect();
    let eager_elapsed = start.elapsed();
    assert!(
        eager_got
            .iter()
            .zip(&got)
            .all(|(eager_tz, lazy_tz)| eager_tz == lazy_tz),
        "eager disagrees with lazy"
    );
    #[expect(
        clippy::cast_precision_loss,
        reason = "preloaded heap bytes ≪ 2^53; KB display"
    )]
    let heap_kb = heap as f64 / 1024.0;
    println!(
        "eager: preload {heap_kb:.1} KB heap in {:.1} ms; lookup {:.2} µs/point (lazy {:.2})",
        ms,
        us_per_point(eager_elapsed),
        us_per_point(lazy_elapsed)
    );

    // every codec must roundtrip to the same answers as the uncompressed finder
    let payload = encode::build_payload(&features, &params)?;
    for codec in [Codec::Gzip, Codec::Zstd, Codec::Brotli, Codec::Xz] {
        let compressed = encode::finish(&payload, codec)?;
        let codec_finder = utz::Finder::from_reader(&compressed[..])
            .unwrap_or_else(|error| panic!("{codec:?} decode failed: {error:?}"));
        assert_eq!(codec_finder.tzbb_release(), "roundtrip-dev");
        for &(lon, lat) in points.iter().take(2_000) {
            assert_eq!(
                codec_finder.lookup(utz::Position { lon, lat }),
                finder.lookup(utz::Position { lon, lat }),
                "{codec:?} ({lon},{lat})"
            );
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "compressed container size ≪ 2^53; KB display"
        )]
        let compressed_kb = compressed.len() as f64 / 1024.0;
        println!("{codec:?}: {compressed_kb:.1} KB, roundtrip OK");
    }
    Ok(())
}

type Ref = (String, Vec<Vec<Vec<(i32, i32)>>>);
fn build_refs(features: &[Feat], qmax: f64) -> Vec<Ref> {
    features
        .iter()
        .map(|feature| {
            let polys = feature
                .polys
                .iter()
                .filter_map(|poly| {
                    let rings: Vec<Vec<(i32, i32)>> = poly
                        .iter()
                        .map(|ring| {
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "|coord·qmax| ≤ qmax < 2^31"
                            )]
                            let mut quantized: Vec<(i32, i32)> = ring
                                .iter()
                                .map(|&(x, y)| {
                                    (
                                        (x / 180.0 * qmax).round() as i32,
                                        (y / 90.0 * qmax).round() as i32,
                                    )
                                })
                                .collect();
                            quantized.dedup();
                            if quantized.first() == quantized.last() && quantized.len() > 1 {
                                quantized.pop();
                            }
                            quantized
                        })
                        .filter(|ring| ring.len() >= 3)
                        .collect();
                    if rings.is_empty() {
                        None
                    } else {
                        Some(rings)
                    }
                })
                .collect();
            (feature.tzid.clone().unwrap_or_default(), polys)
        })
        .collect()
}
fn contains(rings: &[Vec<(i32, i32)>], px: i32, py: i32) -> bool {
    let slices: Vec<&[(i32, i32)]> = rings.iter().map(std::vec::Vec::as_slice).collect();
    utz::pip::contains::<i64, _>(&slices, px, py)
}
fn lookup_linear(refs: &[Ref], px: i32, py: i32) -> Option<String> {
    refs.iter()
        .find(|(_, polys)| polys.iter().any(|poly| contains(poly, px, py)))
        .map(|(tzid, _)| tzid.clone())
}
fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
