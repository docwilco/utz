#!/usr/bin/env bash
# Regenerate every preset asset from its canonical recipe
# (utz_build::Config::<preset>(), driven via `utz-build-cli gen-preset`).
# Assets are gitignored and never committed — CI runs this and the data
# crates include_bytes! the results; their build.rs recipe guards verify
# the headers match. Sources land in the shared cache (cond-GET
# revalidated); the GHS-POP density grid is a ~460 MB download on first
# run.
set -euo pipefail
cd "$(dirname "$0")/.."

for preset in tiny tiny-static compact balanced accurate; do
  cargo run --release -p utz-build-cli -- gen-preset "$preset"
done
