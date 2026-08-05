# utz_data_tiny_static

The μTZ `tiny-static` preset asset. It ships the same decoded payload
as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight floor
1e-3, i16, 2° grid) uncompressed: it costs ~125 KB of flash, loads
zero-copy via `Finder::from_static()` with ~0 RAM and no decoder, and
works on the bare `core` rung.

Regenerate (writes `data/tiny-static.utz`, gitignored) from the canonical
recipe in `utz_common::presets`; the build script refuses assets that
do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset tiny-static
```

License: MIT AND ODbL-1.0
