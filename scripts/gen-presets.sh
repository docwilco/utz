#!/usr/bin/env bash
# Regenerate every preset asset from its canonical recipe (the
# utz_common::presets table, driven via `utz_build_cli gen-preset` with
# no preset name). Assets are gitignored and never committed — CI runs
# this and the data crates include_bytes! the results; their build.rs
# recipe guards verify the headers match. Sources land in the shared
# cache (cond-GET revalidated); the GHS-POP density grid is a ~460 MB
# download on first run.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo run --release -p utz_build_cli -- gen-preset
