#!/usr/bin/env bash
# The single source of truth for repo checks. CI (.github/workflows/ci.yml)
# calls one stage per job; the pre-commit hook (.githooks/pre-commit —
# enable once with `git config core.hooksPath .githooks`) runs `all`.
#
# Compile stages are lists of independent jobs. Locally every job runs
# concurrently in its own target dir (target/checks/<job>): the jobs are
# mostly serial compile+link chains, so a many-core machine absorbs them
# side by side. Under CI=true a stage's jobs run serially in one shared
# dir per stage — few cores, and per-job caches would thrash the runner
# cache. The `docs` job always uses the default target dir so target/doc
# stays the canonical rendered docs (nightly + --cfg docsrs, matching
# docs.rs and the Pages site).
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
  for f in utz_data_tiny/data/tiny.utz utz_data_tiny_static/data/tiny-static.utz \
           utz_data_compact/data/compact.utz utz_data_balanced/data/balanced.utz \
           utz_data_accurate/data/accurate.utz; do
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

# ---- compile jobs: one "name<TAB>command" line each ---------------------

jobs_clippy() {
  # after the workspace pass: feature rungs the workspace build doesn't visit
  printf '%s\t%s\n' \
    clippy-workspace "cargo clippy --workspace --all-targets -- -D warnings" \
    clippy-core-static "cargo clippy -p utz --no-default-features --features core,tiny-static --all-targets -- -D warnings" \
    clippy-all-decoders "cargo clippy -p utz --no-default-features --features std,custom,gzip,ruzstd,brotli,xz,geom-varint-arcs,geom-fixed-width-arcs,geom-full-rings,geom-coarse --all-targets -- -D warnings" \
    clippy-zstd-sys "cargo clippy -p utz --no-default-features --features std,custom,geom-varint-arcs,zstd-sys --all-targets -- -D warnings"
}

jobs_test() {
  # each preset alone (Finder::new() smoke test), then two together (new()
  # cfg'd out, lazy/zero-copy agreement); test-codecs covers every
  # pure-Rust codec's corrupt-stream error test; the C-backed zstd decoder
  # is tested separately (workspace tests use the pure-Rust ruzstd
  # backend); the wasm cdylib is per-build, not via crate-type — see
  # utz_simplify/Cargo.toml
  printf '%s\t%s\n' \
    test-workspace "cargo test --workspace" \
    test-tiny "cargo test -p utz --no-default-features --features std,tiny" \
    test-tiny-static "cargo test -p utz --no-default-features --features core,tiny-static" \
    test-compact "cargo test -p utz --no-default-features --features std,compact" \
    test-balanced "cargo test -p utz --no-default-features --features std,balanced" \
    test-accurate "cargo test -p utz --no-default-features --features std,accurate" \
    test-two-presets "cargo test -p utz --no-default-features --features std,tiny,tiny-static" \
    test-codecs "cargo test -p utz --no-default-features --features std,custom,gzip,ruzstd,brotli,xz,geom-varint-arcs" \
    test-zstd-sys "cargo test -p utz --no-default-features --features std,custom,geom-varint-arcs,zstd-sys" \
    test-wasm-simplify "cargo clippy -p utz_simplify --release --target $WASM -- -D warnings && cargo rustc -p utz_simplify --release --target $WASM --crate-type cdylib" \
    test-wasm-viz "cargo clippy -p utz_viz --no-default-features --release --target $WASM -- -D warnings && cargo rustc -p utz_viz --no-default-features --release --target $WASM --crate-type cdylib"
}

jobs_no_std() {
  # nostd-tiny-static is the headline: preset baked in, zero-copy from
  # flash — no alloc, no codec. Every compressed preset is no_std+alloc
  # clean (pure-Rust decoders). The custom rungs pick decoders explicitly:
  # between the two, all four geometry decoders build on bare metal
  # (presets only ever exercise geom-varint-arcs). The shared bench
  # harness must stay no_std (the embedded firmware uses it).
  printf '%s\t%s\n' \
    nostd-tiny-static "cargo build -p utz --target $BARE_METAL --no-default-features --features core,tiny-static" \
    nostd-tiny "cargo build -p utz --target $BARE_METAL --no-default-features --features alloc,tiny" \
    nostd-compact "cargo build -p utz --target $BARE_METAL --no-default-features --features alloc,compact" \
    nostd-balanced "cargo build -p utz --target $BARE_METAL --no-default-features --features alloc,balanced" \
    nostd-accurate "cargo build -p utz --target $BARE_METAL --no-default-features --features alloc,accurate" \
    nostd-geom-static "cargo build -p utz --target $BARE_METAL --no-default-features --features core,custom,geom-full-rings,geom-coarse" \
    nostd-geom-alloc "cargo build -p utz --target $BARE_METAL --no-default-features --features alloc,custom,gzip,ruzstd,geom-varint-arcs,geom-fixed-width-arcs" \
    nostd-bench-common "cargo build -p utz_bench_common --target $BARE_METAL"
}

jobs_docs() {
  printf '%s\t%s\n' \
    docs "RUSTDOCFLAGS='-D warnings --cfg docsrs' cargo +nightly doc --workspace --no-deps"
}

jobs_all() {
  jobs_clippy
  jobs_test
  jobs_no_std
  jobs_docs
}

# toolchains/targets the jobs assume; installed serially before spawning
# so concurrent jobs never race a rustup install
ensures_for() {
  case "$1" in
    test) ensure_target "$WASM" ;;
    no-std) ensure_target "$BARE_METAL" ;;
    docs) ensure_nightly ;;
    all)
      ensure_target "$WASM"
      ensure_target "$BARE_METAL"
      ensure_nightly
      ;;
  esac
}

run_jobs() {
  local stage=$1 lines
  mapfile -t lines < <("jobs_${stage//-/_}")

  if [[ "${CI:-}" == "true" ]]; then
    local line name cmd
    for line in "${lines[@]}"; do
      name=${line%%$'\t'*}
      cmd=${line#*$'\t'}
      echo "==> checks: $name"
      (
        [[ "$name" != docs ]] && export CARGO_TARGET_DIR="target/checks/$stage"
        eval "$cmd"
      )
    done
    return
  fi

  local pids=() names=() logs=() i=0 failed=0 line name cmd
  for line in "${lines[@]}"; do
    name=${line%%$'\t'*}
    cmd=${line#*$'\t'}
    names[i]=$name
    logs[i]=$(mktemp)
    (
      t0=$SECONDS
      [[ "$name" != docs ]] && export CARGO_TARGET_DIR="target/checks/$name"
      eval "$cmd"
      echo "job wall: $((SECONDS - t0))s"
    ) > "${logs[$i]}" 2>&1 &
    pids[i]=$!
    i=$((i + 1))
  done
  for i in "${!pids[@]}"; do
    if wait "${pids[$i]}"; then
      echo "==> checks: ${names[$i]} ok ($(tail -1 "${logs[$i]}" | grep -o '[0-9]*s$'))"
    else
      failed=1
      echo "==> checks: ${names[$i]} FAILED"
      cat "${logs[$i]}"
    fi
    rm -f "${logs[$i]}"
  done
  return "$failed"
}

run_serial() {
  echo "==> checks: $1"
  "stage_${1//-/_}"
}

case "${1:-all}" in
  presets | fmt | readme) run_serial "$1" ;;
  clippy | test | no-std | docs)
    ensures_for "$1"
    run_jobs "$1"
    ;;
  all)
    for s in presets fmt readme; do run_serial "$s"; done
    ensures_for all
    run_jobs all
    ;;
  *)
    echo "usage: scripts/checks.sh [presets|fmt|readme|clippy|test|no-std|docs|all]" >&2
    exit 2
    ;;
esac
