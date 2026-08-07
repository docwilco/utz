#!/usr/bin/env bash
# Publishes the crates.io set in RELEASING.md's dependency order. Handles
# crates.io's new-crate rate limit (retry with sleep) and waits for the
# registry to serve each crate before publishing its dependents, since
# every later publish resolves the earlier crates from the registry.
# Safe to rerun after a partial run: already-published crates are skipped.
set -uo pipefail
cd "$(dirname "$0")/.."

UA="utz-release (drwilco@gmail.com)"
# The workspace publishes in lockstep; the registry check drops the +tzbb
# build metadata (crates.io stores it, but a prefix match is enough).
VERSION=$(sed -n 's/^version = "\([^+"]*\).*/\1/p' utz/Cargo.toml | head -1)

publish() {
  local crate=$1; shift
  echo "=== publishing $crate $(date -u +%H:%M:%S)"
  local attempt out code
  for attempt in $(seq 1 40); do
    out=$(cargo publish -p "$crate" "$@" 2>&1)
    code=$?
    if [ $code -eq 0 ]; then
      echo "$out" | tail -2
      break
    fi
    if echo "$out" | grep -qi "already exists\|already uploaded"; then
      echo "--- $crate already published, continuing"
      break
    fi
    if echo "$out" | grep -qi "rate limit\|429\|too many\|try again"; then
      echo "--- rate limited on $crate (attempt $attempt); sleeping 120s"
      sleep 120
      continue
    fi
    # transient registry/CDN failures (e.g. a 503 with an empty body from
    # the cache layer on a large upload) deserve a retry, not an abort
    if echo "$out" | grep -qiE "got 50[0-9]|service unavailable|server error|connection reset|timed out"; then
      echo "--- server error on $crate (attempt $attempt); sleeping 30s"
      sleep 30
      continue
    fi
    echo "!!! $crate publish FAILED:"
    echo "$out" | tail -20
    exit 1
  done
  local i
  for i in $(seq 1 120); do
    if curl -s -A "$UA" "https://crates.io/api/v1/crates/$crate/versions" \
        | grep -q "\"num\":\"$VERSION"; then
      echo "--- $crate served by registry"
      return 0
    fi
    sleep 5
  done
  echo "!!! $crate not served after 10 min"
  exit 1
}

# the .utz assets are gitignored but force-included, hence --allow-dirty
# for the data crates; utz's mandatory-feature compile errors fire on the
# zero-feature verify build, hence --no-verify (checks.sh covers the matrix)
publish utz_common
publish utz_simplify
publish utz_encode
publish utz_data_tiny --allow-dirty
publish utz_data_tiny_static --allow-dirty
publish utz_data_compact --allow-dirty
publish utz_data_balanced --allow-dirty
publish utz_data_accurate --allow-dirty
publish utz --no-verify
publish utz_build
publish utz_build_cli
echo "=== ALL PUBLISHED $(date -u +%H:%M:%S)"
