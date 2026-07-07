//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in tiny
//! asset. Run with: cargo test -p utz --no-default-features --features std,tiny

#![cfg(feature = "tiny")]

#[test]
fn new_loads_the_tiny_preset() {
    let f = utz::Finder::new().expect("tiny asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(-0.1276, 51.5072);
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(-0.1276, 51.5072), london, "coarse agrees inland");
}
