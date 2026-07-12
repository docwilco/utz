//! Every geometry encoding of the same recipe answers identically — the
//! build.rs twins cross-checked over the deterministic bench points, lazy
//! and preloaded. Pins the width-dispatch matrix (§14.11): tiny (i16 quant)
//! exercises the narrow i16 eager cache and the `(i16, i16)` image kernel;
//! compact (i24) the i32 eager cache and the `Pack24` image kernel.

use utz_bench_common::assets::{COMPACT_EAGER, COMPACT_FIXED, COMPACT_NONE, TINY_EAGER, TINY_FIXED};

fn agree(name: &str, finders: &[&utz::Finder], pts: &[(f64, f64)]) {
    for &(lon, lat) in pts {
        let pos = utz::Position { lon, lat };
        let want = finders[0].lookup(pos);
        for f in &finders[1..] {
            assert_eq!(f.lookup(pos), want, "{name} disagrees at {pos:?}");
        }
    }
}

#[test]
fn geometry_encodings_agree() {
    let pts = utz_bench_common::gen_pts(2000);

    // tiny recipe (i16 quant): fixed-width arcs streamed, the same preloaded
    // (i16 pairs — half the cache), and the EagerImage twin (i16 pairs
    // folded straight off the payload)
    let tiny_fixed = utz::Finder::from_slice(TINY_FIXED).unwrap();
    let mut tiny_pre = utz::Finder::from_slice(TINY_FIXED).unwrap();
    tiny_pre.preload();
    let tiny_image = utz::Finder::from_static(TINY_EAGER).unwrap();
    agree("tiny", &[&tiny_fixed, &tiny_pre, &tiny_image], &pts);

    // compact recipe (i24 quant): varint lazy, preloaded (i32 pairs), fixed
    // arcs, and the Pack24 image twin
    let compact = utz::Finder::from_slice(COMPACT_NONE).unwrap();
    let mut compact_pre = utz::Finder::from_slice(COMPACT_NONE).unwrap();
    compact_pre.preload();
    let compact_fixed = utz::Finder::from_slice(COMPACT_FIXED).unwrap();
    let compact_image = utz::Finder::from_static(COMPACT_EAGER).unwrap();
    agree("compact", &[&compact, &compact_pre, &compact_fixed, &compact_image], &pts);
}
