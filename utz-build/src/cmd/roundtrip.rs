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

pub fn run(a: Args) -> utz_build::Result<()> {
    let (ds, eps_m, npts) = (a.ds, a.eps_m, a.npts);
    let qbits = 24u32;

    let feats = utz_build::load(&ds)?;
    let p = Params {
        dataset: utz_build::dataset(&ds)?.code(),
        tzbb_release: "roundtrip-dev",
        eps_m,
        quant_bits: qbits,
        grid_deg: 2.0,
        codec: Codec::Uncompressed,
        simplify: encode::SimplifyAlgo::default(),
        geom: encode::GeomEncoding::default(),
    };
    let container = encode::encode(&feats, &p)?;
    #[expect(clippy::cast_precision_loss, reason = "container size ≪ 2^53; KB display")]
    let kb = container.len() as f64 / 1024.0;
    println!("{} container: {kb:.1} KB uncompressed", ds.to_uppercase());

    let finder = utz::Finder::from_reader(&container[..]).expect("decode");
    assert_eq!(finder.tzbb_release(), "roundtrip-dev");

    // reference: linear first-hit over the same quantized geometry
    #[expect(clippy::cast_precision_loss, reason = "qmax = 2^(bits-1)-1 < 2^31, exact in f64")]
    let qmax = ((1u64 << (qbits - 1)) - 1) as f64;
    #[expect(clippy::cast_possible_truncation, reason = "|lon/180·qmax| ≤ qmax < 2^31")]
    let qx = |lon: f64| (lon / 180.0 * qmax).round() as i32;
    #[expect(clippy::cast_possible_truncation, reason = "|lat/90·qmax| ≤ qmax < 2^31")]
    let qy = |lat: f64| (lat / 90.0 * qmax).round() as i32;
    let t = topo::build_topology(&feats, eps_m / 111_320.0);
    let arcs_dq: Vec<Vec<(f64, f64)>> = t.arc_coords.iter()
        .map(|a| {
            let mut q: Vec<(i32, i32)> = a.iter().map(|&(x, y)| (qx(x), qy(y))).collect();
            q.dedup();
            q.iter().map(|&(x, y)| (f64::from(x) / qmax * 180.0, f64::from(y) / qmax * 90.0)).collect()
        })
        .collect();
    let quantized = t.reconstruct(&feats, &arcs_dq);
    let refs = build_refs(&quantized, qmax);

    let pts = gen_pts(npts);
    let t0 = Instant::now();
    let got: Vec<Option<&str>> = pts.iter().map(|&(lo, la)| finder.lookup(utz::Position { lon: lo, lat: la })).collect();
    let dt = t0.elapsed();
    #[expect(clippy::cast_precision_loss, reason = "elapsed µs ≪ 2^53 (would be 285 years); µs/point display")]
    let us = |t: std::time::Duration| t.as_micros() as f64 / npts as f64;

    let (mut diff, mut wrong, mut shown) = (0usize, 0usize, 0usize);
    for (i, &(lo, la)) in pts.iter().enumerate() {
        let (px, py) = (qx(lo), qy(la));
        let want = lookup_linear(&refs, px, py);
        let g = got[i].map(std::string::ToString::to_string);
        if g == want { continue; }
        diff += 1;
        // finder answer valid if its feature actually contains the point
        let ok = g.as_deref().is_some_and(|tz| refs.iter().any(|(t, polys)| t == tz
            && polys.iter().any(|p| contains(p, px, py))));
        if !ok {
            wrong += 1;
            if shown < 8 { shown += 1; println!("  WRONG ({lo:.4},{la:.4}) finder={g:?} linear={want:?}"); }
        }
    }
    println!("disagreements: {diff} ({wrong} wrong, {} benign-overlap)", diff - wrong);
    println!("finder.lookup: {:.2} µs/point over {npts}", us(dt));

    // coarse sanity: must answer everywhere with-oceans covers, cheaply
    let t0 = Instant::now();
    let fz = pts.iter().filter(|&&(lo, la)| finder.lookup_coarse(utz::Position { lon: lo, lat: la }).is_some()).count();
    println!("lookup_coarse: {fz}/{npts} answered, {:.2} µs/point", us(t0.elapsed()));

    // zero-copy static source (core-rung path) must answer identically —
    // lazy lookup streams PIP straight off the borrowed bytes (§9, §14.7)
    let sf = utz::Finder::from_static(Box::leak(container.clone().into_boxed_slice()))
        .expect("static decode");
    let nstatic = npts.min(20_000);
    for &(lo, la) in pts.iter().take(nstatic) {
        assert_eq!(sf.lookup(utz::Position { lon: lo, lat: la }), finder.lookup(utz::Position { lon: lo, lat: la }), "static ({lo},{la})");
    }
    println!("from_static lookup: agrees over {nstatic}");

    // eager mode (§9): preload, must agree everywhere; report heap + speedup
    let mut ef = utz::Finder::from_reader(&container[..]).expect("decode");
    let ((), heap, ms) = super::window_sweep::measure(|| ef.preload());
    let t0 = Instant::now();
    let egot: Vec<Option<&str>> = pts.iter().map(|&(lo, la)| ef.lookup(utz::Position { lon: lo, lat: la })).collect();
    let edt = t0.elapsed();
    assert!(egot.iter().zip(&got).all(|(a, b)| a == b), "eager disagrees with lazy");
    #[expect(clippy::cast_precision_loss, reason = "preloaded heap bytes ≪ 2^53; KB display")]
    let heap_kb = heap as f64 / 1024.0;
    println!(
        "eager: preload {heap_kb:.1} KB heap in {:.1} ms; lookup {:.2} µs/point (lazy {:.2})",
        ms,
        us(edt),
        us(dt)
    );

    // every codec must roundtrip to the same answers as the uncompressed finder
    let payload = encode::build_payload(&feats, &p)?;
    for codec in [Codec::Gzip, Codec::Zstd, Codec::Brotli, Codec::Xz] {
        let c = encode::finish(&payload, codec)?;
        let f = utz::Finder::from_reader(&c[..])
            .unwrap_or_else(|e| panic!("{codec:?} decode failed: {e:?}"));
        assert_eq!(f.tzbb_release(), "roundtrip-dev");
        for &(lo, la) in pts.iter().take(2_000) {
            assert_eq!(f.lookup(utz::Position { lon: lo, lat: la }), finder.lookup(utz::Position { lon: lo, lat: la }), "{codec:?} ({lo},{la})");
        }
        #[expect(clippy::cast_precision_loss, reason = "compressed container size ≪ 2^53; KB display")]
        let ckb = c.len() as f64 / 1024.0;
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
    utz::pip::contains_i64(&slices, px, py)
}
fn lookup_linear(refs: &[Ref], px: i32, py: i32) -> Option<String> {
    refs.iter()
        .find(|(_, polys)| polys.iter().any(|p| contains(p, px, py)))
        .map(|(tz, _)| tz.clone())
}
fn gen_pts(n: usize) -> Vec<(f64, f64)> {
    utz_common::gen_pts(0x1234_5678, n)
}
