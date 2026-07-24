//! Preset-tier smoke test: `Finder::new()` borrows the baked-in
//! tiny-static asset zero-copy — the `core`-rung preset (the utz rlib builds
//! without `alloc`; the test binary itself links std, that's fine).
//! Run with: cargo test -p utz --no-default-features --features core,tiny-static

// mirrors the Finder::new() exactly-one-preset cfg
#![cfg(all(feature = "tiny-static", not(any(feature = "tiny", feature = "compact", feature = "balanced", feature = "accurate"))))]

#[test]
fn garbage_bytes_report_precise_errors() {
    // core rung: errors are fully structured, no captured-text variants exist
    let truncated = utz::Finder::from_static(b"junk").err().expect("too short to parse");
    assert_eq!(truncated, utz::Error::Truncated);
    let bad_magic = utz::Finder::from_static(&[0u8; 16]).err().expect("wrong magic");
    assert_eq!(bad_magic, utz::Error::BadMagic);
}

#[test]
fn new_borrows_the_tiny_static_preset() {
    let f = utz::Finder::new().expect("tiny-static asset parses");
    assert!(!f.tzbb_release().is_empty(), "header carries a TZBB release tag");
    let london = f.lookup(utz::Position { lon: -0.1276, lat: 51.5072 });
    assert!(london.is_some(), "accurate lookup resolves");
    assert_eq!(f.lookup_coarse(utz::Position { lon: -0.1276, lat: 51.5072 }), london, "coarse agrees inland");
}
