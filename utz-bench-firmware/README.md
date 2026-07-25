# utz-bench-firmware

μTZ lookup bench on ESP32-S3 — the on-target flash-latency matrix.

Embeds each preset shape (tiny / compact / balanced) twice — the preset's
compressed asset and its uncompressed twin — and measures every memory
mode the hardware supports:

- **xip-flash**: `Finder::from_static` on the uncompressed blob — lookups
  stream straight out of memory-mapped flash, payload never in RAM.
- **ram**: the uncompressed container copied into heap (`from_vec`) —
  streaming PIP from RAM. Small payloads land in internal SRAM; a
  sacrificial SRAM filler forces a second tiny run into PSRAM, isolating
  the PSRAM access penalty.
- **decode**: `from_slice` on the compressed asset — the buffered-decode
  path (decode time printed separately = per-codec embedded decode speed).
- **eager**: `from_static` + `preload` — geometry decoded to RAM once,
  payload stays in flash.
- **partition**: the same tiny-static asset read back out of a dedicated
  `utzdata` flash partition (found by label in the ESP-IDF partition
  table at runtime) instead of the app image — the ship-the-dataset-
  separately path, e.g. for OTA-ing data without the firmware.

Uses the same harness + points as utz-bench-cli: every leg's checksum must
equal the host run of the same shape at npts=2000.

Setup (once): see README.md. Then `cargo run --release` flashes + monitors.

License: MIT
