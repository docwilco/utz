# utz-data-balanced

μTZ `balanced` preset asset: dataset `now`, RDP
ε=50 m with pop-density weight floor 2e-2, i24, 2/3° grid, brotli.

Regenerate (writes `data/balanced.utz`, gitignored):

```
cargo run --release -p utz-build -- gen now 50 --qbits 24 \
    --w-min 0.020 --grid-deg 0.6666666666666666 --codec brotli \
    -o utz-data-balanced/data/balanced.utz
```

License: MIT
