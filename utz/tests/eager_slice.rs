//! `eager_from_slice` agrees with `from_slice` + `preload` everywhere
//! (decode-to-eager: geometry sections dropped, grid/tzid kept).
//! Run with: cargo test -p utz --no-default-features --features std,tiny

#![cfg(feature = "tiny")]

#[test]
fn eager_from_slice_matches_lazy_and_preload() {
    let lazy = utz::Finder::from_slice(utz::data::TINY).unwrap();
    let mut preloaded = utz::Finder::from_slice(utz::data::TINY).unwrap();
    preloaded.preload();
    let eager = utz::Finder::eager_from_slice(utz::data::TINY).unwrap();
    assert_eq!(
        eager.tzbb_release(),
        lazy.tzbb_release(),
        "header/strings kept"
    );
    // deterministic grid over land and ocean, including cells that need PIP
    let mut hits = 0;
    for i in 0..60u32 {
        for j in 0..30u32 {
            let position = utz::Position {
                lon: -180.0 + (f64::from(i) + 0.37) * 6.0,
                lat: -90.0 + (f64::from(j) + 0.61) * 6.0,
            };
            let want = lazy.lookup(position).expect("grid position in range");
            assert_eq!(eager.lookup(position), Ok(want), "at {position:?}");
            assert_eq!(
                preloaded.lookup(position),
                Ok(want),
                "preload at {position:?}"
            );
            assert_eq!(
                eager.lookup_coarse(position),
                lazy.lookup_coarse(position),
                "coarse at {position:?}"
            );
            hits += usize::from(want.is_some());
        }
    }
    assert_eq!(hits, 1800, "ocean-covered dataset resolves everywhere");
}
