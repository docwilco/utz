# utz-bench-firmware — μTZ lookup bench on ESP32-S3

Runs the shared `utz-bench-common` harness on real hardware. The container is
embedded **uncompressed** in the flash image and borrowed zero-copy through
`Finder::from_static`, so lookups execute straight out of memory-mapped flash
with only the Finder's small scratch state in RAM.

The bench uses the same deterministic points as `utz-bench-cli`; the printed
`checksum` must match the host run for the same container — a cross-platform
correctness check as well as a speed number.

## One-time setup

Xtensa is not in mainline rustc; this crate is excluded from the workspace and
built with the esp toolchain:

```sh
cargo install espup espflash
espup install            # installs the `esp` toolchain (rust-toolchain.toml picks it up)
. ~/export-esp.sh        # or add to your shell profile
```

## Build the container

```sh
cargo run --release -p utz-build -- encode now 500 --codec none -o utz-bench-firmware/container.utz
```

(`--codec none` is required: `from_static` is zero-copy and accepts only
uncompressed containers. `container.utz` is gitignored — regenerate at will.
Try `--w-min 0.052` to bench a population-weighted build.)

## Flash + monitor

```sh
cd utz-bench-firmware
cargo run --release     # espflash flash --monitor (see .cargo/config.toml)
```

Expected output: one result line per loop iteration, e.g.

```
uTZ bench on ESP32-S3 — container 1020 KiB in flash
tzbb release: "dev"
2000 lookups · 2000 hits · … us · … us/lookup · checksum 20758
```

Note: f64 point-in-polygon math is soft-float on the S3 (its FPU is
single-precision), so expect two to three orders of magnitude slower than a
desktop — that gap, and how the grid prefilter shrinks it, is what this
firmware exists to measure.
