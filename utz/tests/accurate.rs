//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in accurate
//! asset. Run with: cargo test -p utz --no-default-features --features std,accurate

// mirrors the Finder::new() exactly-one-preset cfg (§11)
#![cfg(all(feature = "accurate", not(any(feature = "tiny", feature = "tiny-static", feature = "compact", feature = "balanced"))))]

#[test]
fn new_loads_the_accurate_preset() {
    let f = utz::Finder::new().expect("accurate asset decodes");
    assert!(!f.tzbb_release().is_empty(), "header carries a TZBB release tag");
    let london = f.lookup(utz::Position { lon: -0.1276, lat: 51.5072 });
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(utz::Position { lon: -0.1276, lat: 51.5072 }), london, "coarse agrees inland");
}

/// i32 quant is the only tier on the i128 kernel — pin its eager dispatch
/// (the other widths' lazy/eager agreement lives in eager_slice.rs and
/// utz-bench-common's encodings_agree.rs).
#[test]
fn preload_agrees_with_lazy_at_i32_quant() {
    let f = utz::Finder::new().expect("accurate asset decodes");
    let mut pre = utz::Finder::new().expect("accurate asset decodes");
    pre.preload();
    for i in 0..30u32 {
        for j in 0..15u32 {
            let pos = utz::Position {
                lon: -180.0 + (f64::from(i) + 0.37) * 12.0,
                lat: -90.0 + (f64::from(j) + 0.61) * 12.0,
            };
            assert_eq!(pre.lookup(pos), f.lookup(pos), "at {pos:?}");
        }
    }
}
