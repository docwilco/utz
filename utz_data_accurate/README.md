# utz_data_accurate

The μTZ `accurate` preset asset. Its recipe is dataset `all` (the full
Comprehensive zone set), RDP ε=10 m with pop-density weight floor 1e-1,
i32, a 0.5° grid, and brotli; it costs ~8.1 MB of flash, and peak
decode RAM equals the decoded size (~10.6 MB).

Regenerate (writes `data/accurate.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset accurate
```

License: MIT AND ODbL-1.0
