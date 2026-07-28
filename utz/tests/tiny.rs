//! Preset-tier smoke test: `Finder::new()` decodes the baked-in tiny
//! asset. Run with: cargo test -p utz --no-default-features --features std,tiny

// mirrors the Finder::new() exactly-one-preset cfg
#![cfg(all(
    feature = "tiny",
    not(any(
        feature = "tiny-static",
        feature = "compact",
        feature = "balanced",
        feature = "accurate"
    ))
))]

#[test]
fn new_loads_the_tiny_preset() {
    let f = utz::Finder::new().expect("tiny asset decodes");
    assert!(
        !f.tzbb_release().is_empty(),
        "header carries a TZBB release tag"
    );
    let london = f.lookup(utz::Position {
        lon: -0.1276,
        lat: 51.5072,
    });
    // pins the quick-start doctest's claimed value (lib.rs)
    assert_eq!(london, Some("Europe/London"));
    assert_eq!(
        f.lookup_coarse(utz::Position {
            lon: -0.1276,
            lat: 51.5072
        }),
        london,
        "coarse agrees inland"
    );
}
