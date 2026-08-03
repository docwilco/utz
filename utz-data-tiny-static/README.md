# utz-data-tiny-static

μTZ `tiny-static` preset asset: the same decoded
payload as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight
floor 1e-3, i16, 2° grid) shipped uncompressed — ~125 KB flash, zero-copy
via `Finder::from_static()`, ~0 RAM, no decoder; works on the bare `core`
rung.

Regenerate (writes `data/tiny-static.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz-build-cli -- gen-preset tiny-static
```

License: MIT AND ODbL-1.0
