# utz_data_tiny

The μTZ `tiny` preset asset. Its recipe is dataset `now`, RDP
ε=10 000 m with pop-density weight floor 1e-3, i16, a 2° grid, and
gzip; it costs ~71 KB of flash, and peak decode RAM equals the decoded
size (~125 KB).

Regenerate (writes `data/tiny.utz`, gitignored) from the canonical
recipe; the build script refuses assets that do not match it:

```
cargo run --release -p utz_build_cli -- gen-preset tiny
```

License: MIT AND ODbL-1.0
