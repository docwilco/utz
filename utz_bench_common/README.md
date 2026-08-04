# utz_bench_common

The shared μTZ lookup-bench harness. It provides deterministic points,
an injected time source (host `Instant` or the firmware timer), and
elision-proof results. The crate is `no_std` + `alloc` so the exact same
code runs on the CLI and the ESP32-S3 firmware.

License: MIT
