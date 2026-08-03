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
    let london = f
        .lookup(utz::Position {
            lon: -0.1276,
            lat: 51.5072,
        })
        .expect("position in range");
    // pins the quick-start doctest's claimed value (lib.rs)
    assert_eq!(london, Some("Europe/London"));
    // provenance: the tiny recipe's density-weight floor round-trips
    assert_eq!(f.density_weight_floor(), Some(0.001));
    assert_eq!(
        f.lookup_coarse(utz::Position {
            lon: -0.1276,
            lat: 51.5072
        }),
        Ok(london),
        "coarse agrees inland"
    );
}

#[test]
fn invalid_positions_error() {
    let f = utz::Finder::new().expect("tiny asset parses");
    for pos in [
        utz::Position {
            lon: 200.0,
            lat: 0.0,
        },
        utz::Position {
            lon: 0.0,
            lat: -90.5,
        },
        utz::Position {
            lon: f64::NAN,
            lat: 0.0,
        },
        utz::Position {
            lon: 0.0,
            lat: f64::NAN,
        },
    ] {
        assert!(!pos.is_valid());
        assert_eq!(f.lookup(pos), Err(utz::Error::InvalidPosition));
        assert_eq!(f.lookup_coarse(pos), Err(utz::Error::InvalidPosition));
    }
    // the unchecked variants stay memory-safe on wild input (documented
    // to answer with an arbitrary nearby zone)
    let wild = utz::Position {
        lon: 999.0,
        lat: 999.0,
    };
    let _ = f.lookup_unchecked(wild);
    let _ = f.lookup_coarse_unchecked(wild);
    // the domain corners are valid
    assert!(utz::Position {
        lon: 180.0,
        lat: 90.0
    }
    .is_valid());
    assert!(utz::Position {
        lon: -180.0,
        lat: -90.0
    }
    .is_valid());
}
