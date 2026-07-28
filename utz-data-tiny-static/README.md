# utz-data-tiny-static

μTZ `tiny-static` preset asset: the SAME decoded
container as `tiny` (dataset `now`, RDP ε=10 000 m with pop-density weight
floor 1e-3, i16, 2° grid) shipped uncompressed — ~125 K flash, zero-copy
via `Finder::from_static()`, ~0 RAM, no decoder; works on the bare `core`
rung.

Regenerate (writes `data/tiny-static.utz`, gitignored):

```
cargo run --release -p utz-build-cli -- gen now 10000 --qbits 16 \
    --w-min 0.001 --codec none -o utz-data-tiny-static/data/tiny-static.utz
```

License: MIT
