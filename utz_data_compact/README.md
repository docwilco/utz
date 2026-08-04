# utz_data_compact

The μTZ `compact` preset asset. Its recipe is dataset `now`, RDP
ε=1 000 m with pop-density weight floor 1e-3, i24, a 4/3° grid, and xz;
it costs ~445 KB of flash, and peak decode RAM equals the decoded size
(~608 KB).

Regenerate (writes `data/compact.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset compact
```

License: MIT AND ODbL-1.0
