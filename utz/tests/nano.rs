//! Preset-tier smoke test (§11): `Finder::new()` decodes the baked-in nano
//! asset. Run with: cargo test -p utz --no-default-features --features nano

#![cfg(feature = "nano")]

#[test]
fn new_loads_the_nano_preset() {
    let f = utz::Finder::new().expect("nano asset decodes");
    assert_eq!(f.tzbb_release(), "dev");
    let london = f.lookup(-0.1276, 51.5072);
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(-0.1276, 51.5072), london, "coarse agrees inland");
}
