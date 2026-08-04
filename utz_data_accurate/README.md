# utz_data_accurate

μTZ `accurate` preset asset: dataset `all` (the
full Comprehensive zone set), RDP ε=10 m with pop-density weight floor
1e-1, i32, 0.5° grid, brotli — ~8.1 MB flash, peak decode RAM =
decoded size (~10.6 MB).

Regenerate (writes `data/accurate.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset accurate
```

License: MIT AND ODbL-1.0
