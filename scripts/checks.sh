#!/usr/bin/env bash
# The single source of truth for repo checks. CI (.github/workflows/ci.yml)
# calls one stage per job; the pre-commit hook (.githooks/pre-commit —
# enable once with `git config core.hooksPath .githooks`) runs `all`.
set -euo pipefail
cd "$(dirname "$0")/.."

BARE_METAL=riscv32imac-unknown-none-elf
WASM=wasm32-unknown-unknown

ensure_target() {
  rustup target list --installed | grep -qx "$1" || rustup target add "$1"
}

ensure_nightly() {
  rustup toolchain list | grep -q '^nightly' || rustup toolchain install nightly --profile minimal
}

# Preset assets are gitignored and regenerated, never committed. CI's gen
# job builds them once per run and hands them to the other jobs as an
# artifact; locally, regenerate only if some are missing.
stage_presets() {
  local f missing=()
  for f in utz-data-tiny/data/tiny.utz utz-data-tiny-static/data/tiny-static.utz \
           utz-data-compact/data/compact.utz utz-data-balanced/data/balanced.utz \
           utz-data-accurate/data/accurate.utz; do
    [[ -f "$f" ]] || missing+=("$f")
  done
  if ((${#missing[@]})); then
    echo "regenerating missing preset assets: ${missing[*]}"
    ./scripts/gen-presets.sh
  fi
}

stage_fmt() {
  cargo fmt --check || {
    echo
    echo "Formatting check failed. Run 'cargo fmt' to fix."
    return 1
  }
}

stage_readme() { ./scripts/gen-readme.sh --check; }

stage_clippy() {
  cargo clippy --workspace --all-targets -- -D warnings
  # feature rungs the workspace build doesn't visit
  cargo clippy -p utz --no-default-features --features core,tiny-static --all-targets -- -D warnings
  cargo clippy -p utz --no-default-features --features std,custom,gzip,ruzstd,brotli,xz,geom-varint-arcs,geom-fixed-width-arcs,geom-full-rings,geom-coarse --all-targets -- -D warnings
  cargo clippy -p utz --no-default-features --features std,custom,geom-varint-arcs,zstd-sys --all-targets -- -D warnings
}

stage_test() {
  cargo test --workspace
  # each preset alone (Finder::new() smoke test), then two together
  # (new() cfg'd out, lazy/zero-copy agreement)
  cargo test -p utz --no-default-features --features std,tiny
  cargo test -p utz --no-default-features --features core,tiny-static
  cargo test -p utz --no-default-features --features std,compact
  cargo test -p utz --no-default-features --features std,balanced
  cargo test -p utz --no-default-features --features std,accurate
  cargo test -p utz --no-default-features --features std,tiny,tiny-static
  # every pure-Rust codec's corrupt-stream error test, no preset assets needed
  cargo test -p utz --no-default-features --features std,custom,gzip,ruzstd,brotli,xz,geom-varint-arcs
  # the C-backed zstd decoder feature is tested separately
  # (workspace tests use the pure-Rust ruzstd backend)
  cargo test -p utz --no-default-features --features std,custom,geom-varint-arcs,zstd-sys
  # cdylib per-build, not via crate-type — see utz-simplify/Cargo.toml
  ensure_target "$WASM"
  cargo rustc -p utz-simplify --release --target "$WASM" --crate-type cdylib
}

stage_no_std() {
  ensure_target "$BARE_METAL"
  # the headline: preset baked in, zero-copy from flash — no alloc, no codec
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features core,tiny-static
  # every compressed preset is no_std+alloc clean (pure-Rust decoders)
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features alloc,tiny
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features alloc,compact
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features alloc,balanced
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features alloc,accurate
  # bring-your-own-asset rungs: custom carries no decoder, so pick them
  # here — between the two lines all four geometry decoders build on
  # bare metal (presets only ever exercise geom-varint-arcs)
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features core,custom,geom-full-rings,geom-coarse
  cargo build -p utz --target "$BARE_METAL" --no-default-features --features alloc,custom,gzip,ruzstd,geom-varint-arcs,geom-fixed-width-arcs
  # the shared bench harness must stay no_std (the embedded firmware uses it)
  cargo build -p utz-bench-common --target "$BARE_METAL"
}

# nightly + --cfg docsrs: feature-gate banners render (doc_cfg), and
# target/doc matches what docs.rs and the Pages site publish
stage_docs() {
  ensure_nightly
  RUSTDOCFLAGS="-D warnings --cfg docsrs" cargo +nightly doc --workspace --no-deps
}

# The compile stages run concurrently under `all`; a private target dir
# per stage keeps them off each other's cargo build lock. `docs` keeps
# the default dir so target/doc stays the canonical rendered docs.
stage_dir() {
  case "$1" in
    clippy | test | no-std) export CARGO_TARGET_DIR="target/checks/$1" ;;
  esac
}

run() {
  echo "==> checks: $1"
  (
    stage_dir "$1"
    "stage_${1//-/_}"
  )
}

run_compile_stages_concurrently() {
  local stages=(clippy test no-std docs) pids=() logs=() failed=0 i
  for i in "${!stages[@]}"; do
    logs[i]=$(mktemp)
    run "${stages[$i]}" > "${logs[$i]}" 2>&1 &
    pids[i]=$!
  done
  for i in "${!stages[@]}"; do
    if wait "${pids[$i]}"; then
      echo "==> checks: ${stages[$i]} ok"
    else
      failed=1
      echo "==> checks: ${stages[$i]} FAILED"
      cat "${logs[$i]}"
    fi
    rm -f "${logs[$i]}"
  done
  return "$failed"
}

case "${1:-all}" in
  presets | fmt | readme | clippy | test | no-std | docs) run "$1" ;;
  all)
    for s in presets fmt readme; do run "$s"; done
    run_compile_stages_concurrently
    ;;
  *)
    echo "usage: scripts/checks.sh [presets|fmt|readme|clippy|test|no-std|docs|all]" >&2
    exit 2
    ;;
esac
