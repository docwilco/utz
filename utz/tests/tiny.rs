//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in tiny
//! asset. Run with: cargo test -p utz --no-default-features --features std,tiny

// mirrors the Finder::new() exactly-one-preset cfg (§11)
#![cfg(all(feature = "tiny", not(any(feature = "tiny-static", feature = "compact", feature = "balanced", feature = "accurate"))))]

#[test]
fn new_loads_the_tiny_preset() {
    let f = utz::Finder::new().expect("tiny asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(utz::Position { lon: -0.1276, lat: 51.5072 });
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(utz::Position { lon: -0.1276, lat: 51.5072 }), london, "coarse agrees inland");
}
