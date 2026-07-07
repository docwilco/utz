#!/usr/bin/env bash
# Regenerate every preset asset (PLAN.md §11; recipes pinned in §14.5).
# Assets are gitignored and never committed — CI runs this and the data
# crates include_bytes! the results. Sources land in cache/ (cond-GET
# revalidated); the GHS-POP density grid is a ~460 MB download on first run.
set -euo pipefail
cd "$(dirname "$0")/.."

gen() { cargo run --release -p utz-build -- gen "$@"; }

gen now 10000 --qbits 16 --w-min 0.001 --codec gzip -o utz-data-tiny/data/tiny.utz
gen now 10000 --qbits 16 --w-min 0.001 --codec none -o utz-data-tiny-static/data/tiny-static.utz
gen now 1000 --qbits 24 --w-min 0.001 --grid-deg 1.3333333333333333 --codec xz -o utz-data-compact/data/compact.utz
gen now 50 --qbits 24 --w-min 0.020 --grid-deg 0.6666666666666666 --codec brotli -o utz-data-balanced/data/balanced.utz
gen now 10 --qbits 32 --w-min 0.10 --grid-deg 0.5 --codec brotli -o utz-data-accurate/data/accurate.utz
