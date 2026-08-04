# utz_bench_firmware

The μTZ lookup bench on ESP32-S3: the on-target flash-latency matrix.

The firmware embeds each preset shape (tiny / compact / balanced) twice
(the preset's compressed asset and its uncompressed twin) and measures
every memory mode the hardware supports:

- **xip-flash** calls `Finder::from_static()` on the uncompressed blob;
  lookups stream straight out of memory-mapped flash, and the payload is
  never in RAM.
- **ram** copies the uncompressed container into the heap (`from_vec()`)
  and streams PIP from RAM. Small payloads land in internal SRAM; a
  sacrificial SRAM filler forces a second tiny run into PSRAM, isolating
  the PSRAM access penalty.
- **decode** calls `from_slice()` on the compressed asset, the
  buffered-decode path; the decode time is printed separately and gives
  the per-codec embedded decode speed.
- **eager** runs `from_static()` plus `preload()`; geometry is decoded to
  RAM once, and the payload stays in flash.
- **partition** reads the same tiny-static asset back out of a dedicated
  `utzdata` flash partition (found by label in the ESP-IDF partition
  table at runtime) instead of the app image; this is the
  ship-the-dataset-separately path, e.g. for OTA-ing data without the
  firmware.

The bench uses the same harness and points as `utz_bench_cli`: every
leg's checksum must equal the host run of the same shape at npts=2000.

One-time setup is described in README.md. After that,
`cargo run --release` flashes and monitors.

License: MIT
