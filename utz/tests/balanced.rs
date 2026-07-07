//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in balanced
//! asset. Run with: cargo test -p utz --no-default-features --features std,balanced

// mirrors the Finder::new() exactly-one-preset cfg (§11)
#![cfg(all(feature = "balanced", not(any(feature = "tiny", feature = "tiny-static", feature = "compact", feature = "accurate"))))]

#[test]
fn new_loads_the_balanced_preset() {
    let f = utz::Finder::new().expect("balanced asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(-0.1276, 51.5072);
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(-0.1276, 51.5072), london, "coarse agrees inland");
}
