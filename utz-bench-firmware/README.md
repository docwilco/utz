# utz-bench-firmware — μTZ lookup bench on ESP32-S3

Runs the shared `utz-bench-common` harness on real hardware, covering the
PLAN §15 memory-mode matrix for each preset shape (tiny / compact / balanced):

- **xip-flash** — uncompressed container borrowed zero-copy from
  memory-mapped flash (`Finder::from_static`); payload never in RAM.
- **ram** — the same container copied to heap (`from_vec`): streaming PIP
  from RAM. Tiny runs twice, once from internal SRAM and once forced into
  PSRAM, isolating the PSRAM access penalty.
- **decode** — the preset's compressed asset decoded from flash into heap
  (`from_slice`); the decode time doubles as a per-codec embedded speed number.
- **eager** — `from_static` + `preload()`: payload in flash, geometry cache
  in RAM.

The bench uses the same deterministic points as `utz-bench-cli`; every leg's
printed `checksum` must match the host run for the same shape and npts — a
cross-platform correctness check as well as a speed number.

## One-time setup

Xtensa is not in mainline rustc; this crate is excluded from the workspace and
built with the esp toolchain:

```sh
cargo install espup espflash
espup install            # installs the `esp` toolchain (rust-toolchain.toml picks it up)
. ~/export-esp.sh        # or add to your shell profile
```

## Build the containers

The six embedded blobs are the preset assets plus uncompressed twins of the
compact/balanced shapes (`from_static` accepts only codec *none*; all blobs
are gitignored — regenerate at will):

```sh
scripts/gen-presets.sh   # tiny.utz, tiny-static.utz, compact.utz, balanced.utz
cp utz-data-tiny/data/tiny.utz utz-data-tiny-static/data/tiny-static.utz \
   utz-data-compact/data/compact.utz utz-data-balanced/data/balanced.utz utz-bench-firmware/
cargo run --release -p utz-build -- gen now 1000 --qbits 24 --w-min 0.001 --grid-deg 1.3333333333333333 --codec none -o utz-bench-firmware/compact-none.utz
cargo run --release -p utz-build -- gen now 50   --qbits 24 --w-min 0.020 --grid-deg 0.6666666666666666  --codec none -o utz-bench-firmware/balanced-none.utz
```

## Flash + monitor

```sh
cd utz-bench-firmware
cargo run --release     # espflash flash --monitor (see .cargo/config.toml)
```

One `RESULT` line per leg (plus `INFO` decode/preload timings and payload
placement, `SKIP` where a leg doesn't fit the detected memory), then `DONE`.
Compare against the host at the same point count:

```sh
cargo run --release -p utz-bench-cli -- utz-data-tiny/data/tiny.utz 2000
```

Note: expect two to three orders of magnitude slower than a desktop — not
floats (PIP is integer i64; f64 only touches the ~20-op quantize/grid
boundary, soft-float on the S3's f32-only FPU but negligible) but scalar
integer throughput: a 240 MHz in-order 32-bit core doing 64-bit math and,
in streaming modes, per-vertex varint decode. That gap, and how little the
memory mode matters next to it, is what this firmware exists to measure.
