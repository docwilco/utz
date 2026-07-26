# utz-bench-cli

Host-side μTZ lookup bench: same harness (utz-bench-common) and the same
deterministic points as the ESP32-S3 firmware, so host and target numbers
(and answer checksums) are directly comparable.

    cargo run --release -p utz-bench-cli -- <shape|container.utz> [npts] [rounds]

A shape name picks an embedded container: the presets (`tiny`,
`tiny-static`, `compact`, `balanced`, `accurate`; the utz-data-* crates,
via the `utz` preset features) or a generated custom shape
(`compact-none`, `balanced-none`, and `tiny-fixed-static`: tiny-static
with fixed-width arcs, the XIP speed tier). Anything else is read as a
`.utz` file path.

License: MIT
