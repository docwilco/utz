# utz-data-accurate

μTZ `accurate` preset asset: dataset `all` (the
full Comprehensive zone set), RDP ε=10 m with pop-density weight floor
1e-1, i32, 0.5° grid, brotli — ~8.1 MB flash, peak decode RAM =
decoded size (~10.6 MB).

Regenerate (writes `data/accurate.utz`, gitignored):

```
cargo run --release -p utz-build-cli -- gen all 10 --qbits 32 \
    --w-min 0.10 --grid-deg 0.5 --codec brotli \
    -o utz-data-accurate/data/accurate.utz
```

License: MIT AND ODbL-1.0
