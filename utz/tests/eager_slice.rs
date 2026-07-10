//! `eager_from_slice` agrees with `from_slice` + `preload` everywhere
//! (§14.10 decode-to-eager: geometry sections dropped, grid/tzid kept).
//! Run with: cargo test -p utz --no-default-features --features std,tiny

#![cfg(feature = "tiny")]

#[test]
fn eager_from_slice_matches_lazy_and_preload() {
    let lazy = utz::Finder::from_slice(utz::data::TINY).unwrap();
    let mut pre = utz::Finder::from_slice(utz::data::TINY).unwrap();
    pre.preload();
    let eager = utz::Finder::eager_from_slice(utz::data::TINY).unwrap();
    assert_eq!(eager.tzbb_release(), lazy.tzbb_release(), "header/strings kept");
    // deterministic grid over land and ocean, including cells that need PIP
    let mut n = 0;
    for i in 0..60u32 {
        for j in 0..30u32 {
            let pos = utz::Position {
                lon: -180.0 + (f64::from(i) + 0.37) * 6.0,
                lat: -90.0 + (f64::from(j) + 0.61) * 6.0,
            };
            let want = lazy.lookup(pos);
            assert_eq!(eager.lookup(pos), want, "at {pos:?}");
            assert_eq!(pre.lookup(pos), want, "preload at {pos:?}");
            assert_eq!(eager.lookup_coarse(pos), lazy.lookup_coarse(pos), "coarse at {pos:?}");
            n += usize::from(want.is_some());
        }
    }
    assert!(n > 1000, "grid should mostly resolve ({n} hits)");
}
