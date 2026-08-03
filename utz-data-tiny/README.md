# utz-data-tiny

μTZ `tiny` preset asset: dataset `now`, RDP
ε=10 000 m with pop-density weight floor 1e-3, i16, 2° grid, gzip —
~71 KB flash, peak decode RAM = decoded size (~125 KB).

Regenerate (writes `data/tiny.utz`, gitignored):

```
cargo run --release -p utz-build-cli -- gen now 10000 --qbits 16 \
    --w-min 0.001 --codec gzip -o utz-data-tiny/data/tiny.utz
```

License: MIT AND ODbL-1.0
