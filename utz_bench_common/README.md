# utz_bench_common

Shared μTZ lookup-bench harness: deterministic points, an injected time
source (host `Instant` / firmware timer), and elision-proof results.
`no_std` + `alloc` so the exact same code runs on the CLI and the
ESP32-S3 firmware.

License: MIT
