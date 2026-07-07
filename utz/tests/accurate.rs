//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in accurate
//! asset. Run with: cargo test -p utz --no-default-features --features std,accurate

// mirrors the Finder::new() exactly-one-preset cfg (§11)
#![cfg(all(feature = "accurate", not(any(feature = "tiny", feature = "tiny-static", feature = "compact", feature = "balanced"))))]

#[test]
fn new_loads_the_accurate_preset() {
    let f = utz::Finder::new().expect("accurate asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(utz::Position { lon: -0.1276, lat: 51.5072 });
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(utz::Position { lon: -0.1276, lat: 51.5072 }), london, "coarse agrees inland");
}
