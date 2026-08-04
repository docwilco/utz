# utz_data_balanced

μTZ `balanced` preset asset: dataset `now`, RDP
ε=50 m with pop-density weight floor 2e-2, i24, 2/3° grid, brotli —
~1.2 MB flash, peak decode RAM = decoded size (~1.9 MB).

Regenerate (writes `data/balanced.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset balanced
```

License: MIT AND ODbL-1.0
