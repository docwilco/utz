// End-to-end roundtrip: encode the real container, decode with the runtime
// Finder, and validate lookup() against a linear first-hit PIP scan over the
// same quantized geometry (the grid_bench reference).
//
// usage: utz-build roundtrip [ds] [eps_m] [npts]

use std::time::Instant;

use utz_build::encode::{self, Codec, Params};
use utz_build::{topo, Feat};

#[derive(clap::Args)]
pub struct Args {
    /// dataset: [land-]now|1970|all
    #[arg(default_value = "now")]
    ds: String,
    /// simplification tolerance in meters
    #[arg(default_value_t = 500.0)]
    eps_m: f64,
    /// number of sample points
    #[arg(default_value_t = 100_000)]
    npts: usize,
}

pub fn run(args: Args) -> utz_build::Result<()> {
    let (dataset, eps_m, npts) = (args.ds, args.eps_m, args.npts);
    let qbits = 24u32;

    let feats = utz_build::load(&dataset)?;
    let params = Params {
        dataset: utz_build::dataset(&dataset)?.code(),
        tzbb_release: "roundtrip-dev",
        eps_m,
        quant_bits: qbits,
        grid_deg: 2.0,
        codec: Codec::Uncompressed,
        simplify: encode::SimplifyAlgo::default(),
        geom: encode::GeomEncoding::default(),
    };
    let container = encode::encode(&feats, &params)?;
    #[expect(clippy::cast_precision_loss, reason = "container size ≪ 2^53; KB display")]
    let kb = container.len() as f64 / 1024.0;
    println!("{} container: {kb:.1} KB uncompressed", dataset.to_uppercase());

    let finder = utz::Finder::from_reader(&container[..]).expect("decode");
    assert_eq!(finder.tzbb_release(), "roundtrip-dev");

    // reference: linear first-hit over the same quantized geometry
    #[expect(clippy::cast_precision_loss, reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64")]
    let qmax = ((1u64 << (qbits - 1)) - 1) as f64;
    #[expect(clippy::cast_possible_truncation, reason = "|lon/180·qmax| ≤ qmax < 2^31")]
    let quantize_lon = |lon: f64| (lon / 180.0 * qmax).round() as i32;
    #[expect(clippy::cast_possible_truncation, reason = "|lat/90·qmax| ≤ qmax < 2^31")]
    let quantize_lat = |lat: f64| (lat / 90.0 * qmax).round() as i32;
    let topology = topo::build_topology(&feats, eps_m / 111_320.0);
    let arcs_dq: Vec<Vec<(f64, f64)>> = topology.arc_coords.iter()
        .map(|arc| {
            let mut qcoords: Vec<(i32, i32)> = arc.iter().map(|&(lon, lat)| (quantize_lon(lon), quantize_lat(lat))).collect();
            qcoords.dedup();
            qcoords.iter().map(|&(qlon, qlat)| (f64::from(qlon) / qmax * 180.0, f64::from(qlat) / qmax * 90.0)).collect()
        })
        .collect();
    let quantized = topology.reconstruct(&feats, &arcs_dq);
    let refs = build_refs(&quantized, qmax);

    let pts = gen_pts(npts);
    let start = Instant::now();
    let got: Vec<Option<&str>> = pts.iter().map(|&(lon, lat)| finder.lookup(utz::Position { lon, lat })).collect();
    let lazy_elapsed = start.elapsed();
    #[expect(clippy::cast_precision_loss, reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/point display")]
    let us_per_point = |elapsed: std::time::Duration| elapsed.as_micros() as f64 / npts as f64;

    let (mut diff, mut wrong, mut shown) = (0usize, 0usize, 0usize);
    for (idx, &(lon, lat)) in pts.iter().enumerate() {
        let (px, py) = (quantize_lon(lon), quantize_lat(lat));
        let want = lookup_linear(&refs, px, py);
        let finder_tz = got[idx].map(std::string::ToString::to_string);
        if finder_tz == want { continue; }
        diff += 1;
        // finder answer valid if its feature actually contains the point
        let ok = finder_tz.as_deref().is_some_and(|tz| refs.iter().any(|(ref_tz, polys)| ref_tz == tz
            && polys.iter().any(|poly| contains(poly, px, py))));
        if !ok {
            wrong += 1;
            if shown < 8 { shown += 1; println!("  WRONG ({lon:.4},{lat:.4}) finder={finder_tz:?} linear={want:?}"); }
        }
    }
    println!("disagreements: {diff} ({wrong} wrong, {} benign-overlap)", diff - wrong);
    println!("finder.lookup: {:.2} µs/point over {npts}", us_per_point(lazy_elapsed));

    // coarse sanity: must answer everywhere with-oceans covers, cheaply
    let start = Instant::now();
    let answered = pts.iter().filter(|&&(lon, lat)| finder.lookup_coarse(utz::Position { lon, lat }).is_some()).count();
    println!("lookup_coarse: {answered}/{npts} answered, {:.2} µs/point", us_per_point(start.elapsed()));

    // zero-copy static source (core-rung path) must answer identically —
    // lazy lookup streams PIP straight off the borrowed bytes (§9, §14.7)
    let static_finder = utz::Finder::from_static(Box::leak(container.clone().into_boxed_slice()))
        .expect("static decode");
    let nstatic = npts.min(20_000);
    for &(lon, lat) in pts.iter().take(nstatic) {
        assert_eq!(static_finder.lookup(utz::Position { lon, lat }), finder.lookup(utz::Position { lon, lat }), "static ({lon},{lat})");
    }
    println!("from_static lookup: agrees over {nstatic}");

    // eager mode (§9): preload, must agree everywhere; report heap + speedup
    let mut eager_finder = utz::Finder::from_reader(&container[..]).expect("decode");
    let ((), heap, ms) = super::window_sweep::measure(|| eager_finder.preload());
    let start = Instant::now();
    let eager_got: Vec<Option<&str>> = pts.iter().map(|&(lon, lat)| eager_finder.lookup(utz::Position { lon, lat })).collect();
    let eager_elapsed = start.elapsed();
    assert!(eager_got.iter().zip(&got).all(|(eager_tz, lazy_tz)| eager_tz == lazy_tz), "eager disagrees with lazy");
    #[expect(clippy::cast_precision_loss, reason = "preloaded heap bytes ≪ 2^53; KB display")]
    let heap_kb = heap as f64 / 1024.0;
    println!(
        "eager: preload {heap_kb:.1} KB heap in {:.1} ms; lookup {:.2} µs/point (lazy {:.2})",
        ms,
        us_per_point(eager_elapsed),
        us_per_point(lazy_elapsed)
    );

    // every codec must roundtrip to the same answers as the uncompressed finder
    let payload = encode::build_payload(&feats, &params)?;
    for codec in [Codec::Gzip, Codec::Zstd, Codec::Brotli, Codec::Xz] {
        let compressed = encode::finish(&payload, codec)?;
        let codec_finder = utz::Finder::from_reader(&compressed[..])
            .unwrap_or_else(|err| panic!("{codec:?} decode failed: {err:?}"));
        assert_eq!(codec_finder.tzbb_release(), "roundtrip-dev");
        for &(lon, lat) in pts.iter().take(2_000) {
            assert_eq!(codec_finder.lookup(utz::Position { lon, lat }), finder.lookup(utz::Position { lon, lat }), "{codec:?} ({lon},{lat})");
        }
        #[expect(clippy::cast_precision_loss, reason = "compressed container size ≪ 2^53; KB display")]
        let ckb = compressed.len() as f64 / 1024.0;
        println!("{codec:?}: {ckb:.1} KB, roundtrip OK");
    }
    Ok(())
}

type Ref = (String, Vec<Vec<Vec<(i32, i32)>>>);
fn build_refs(feats: &[Feat], qmax: f64) -> Vec<Ref> {
    feats.iter().map(|f| {
        let polys = f.polys.iter().filter_map(|p| {
            let rings: Vec<Vec<(i32, i32)>> = p.iter().map(|r| {
                #[expect(clippy::cast_possible_truncation, reason = "|coord·qmax| ≤ qmax < 2^31")]
                let mut q: Vec<(i32, i32)> = r.iter()
                    .map(|&(x, y)| ((x / 180.0 * qmax).round() as i32, (y / 90.0 * qmax).round() as i32))
                    .collect();
                q.dedup();
                if q.first() == q.last() && q.len() > 1 { q.pop(); }
                q
            }).filter(|r| r.len() >= 3).collect();
            if rings.is_empty() { None } else { Some(rings) }
        }).collect();
        (f.tzid.clone().unwrap_or_default(), polys)
    }).collect()
}
fn contains(rings: &[Vec<(i32, i32)>], px: i32, py: i32) -> bool {
    let slices: Vec<&[(i32, i32)]> = rings.iter().map(std::vec::Vec::as_slice).collect();
    utz::pip::contains::<i64, _>(&slices, px, py)
}
fn lookup_linear(refs: &[Ref], px: i32, py: i32) -> Option<String> {
    refs.iter()
        .find(|(_, polys)| polys.iter().any(|p| contains(p, px, py)))
        .map(|(tz, _)| tz.clone())
}
fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
