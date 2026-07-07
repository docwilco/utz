//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in compact
//! asset. Run with: cargo test -p utz --no-default-features --features std,compact

// mirrors the Finder::new() exactly-one-preset cfg (§11)
#![cfg(all(feature = "compact", not(any(feature = "tiny", feature = "tiny-static", feature = "balanced", feature = "accurate"))))]

#[test]
fn new_loads_the_compact_preset() {
    let f = utz::Finder::new().expect("compact asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(-0.1276, 51.5072);
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(-0.1276, 51.5072), london, "coarse agrees inland");
}
