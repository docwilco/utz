# utz_data_balanced

The μTZ `balanced` preset asset. Its recipe is dataset `now`, RDP
ε=50 m with pop-density weight floor 2e-2, i24, a 2/3° grid, and
brotli; it costs ~1.2 MB of flash, and peak decode RAM equals the
decoded size (~1.9 MB).

Regenerate (writes `data/balanced.utz`, gitignored) from the canonical
recipe in `utz_common::presets`; the build script refuses assets that
do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset balanced
```

License: MIT AND ODbL-1.0
