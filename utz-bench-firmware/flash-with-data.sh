#!/usr/bin/env bash
# cargo runner (.cargo/config.toml): write the utzdata partition, then flash
# the app + custom partition table (espflash.toml) and monitor. The address
# must match the utzdata offset in partitions.csv. The asset is the
# tiny-static preset — byte-identical to the embedded TINY_NONE, so the
# firmware can verify the partition read against its twin.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
espflash write-bin 0xc00000 "$here/../utz-data-tiny-static/data/tiny-static.utz"
exec espflash flash --monitor "$@"
