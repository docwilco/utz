//! Multi-preset build (§11): with two presets in the tree `Finder::new()` is
//! cfg'd out — consumers load explicitly, and the compressed and flat tiny
//! variants must answer identically (same decoded container).
//! Run with: cargo test -p utz --no-default-features --features std,tiny,tiny-static

#![cfg(all(feature = "tiny", feature = "tiny-static"))]

#[test]
fn tiny_and_tiny_static_agree() {
    let lazy = utz::Finder::from_slice(utz::data::TINY).expect("tiny decodes");
    let flat = utz::Finder::from_static(utz::data::TINY_STATIC).expect("tiny-static parses");
    // flat trades flash for zero decode RAM
    assert!(utz::data::TINY_STATIC.len() > utz::data::TINY.len());
    for (lon, lat) in [
        (-0.1276, 51.5072),  // London
        (139.6917, 35.6895), // Tokyo
        (-74.006, 40.7128),  // New York
        (151.2093, -33.8688),// Sydney
        (77.209, 28.6139),   // Delhi
        (0.0, 0.0),          // gulf of Guinea (ocean)
    ] {
        assert_eq!(lazy.lookup(utz::Position { lon, lat }), flat.lookup(utz::Position { lon, lat }), "({lon}, {lat})");
    }
}
