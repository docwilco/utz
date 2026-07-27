#!/usr/bin/env bash
# <crate>/README.md = the crate's top doc comment (src/lib.rs, else
# src/main.rs) rendered by cargo-readme; a crate-local README.tpl (utz has
# one) customizes the frame.
#
# Usage: gen-readme.sh [--check|--list] [crate]   (no crate = all crates)
#   --check  compare instead of writing (CI mode)
#   --list   print the crate list (the pre-commit hook iterates it)
set -euo pipefail
cd "$(dirname "$0")/.."

CRATES=(
  utz utz-common utz-encode utz-build utz-build-cli utz-simplify
  utz-data-tiny utz-data-tiny-static utz-data-compact
  utz-data-balanced utz-data-accurate
  utz-bench-common utz-bench-cli utz-bench-firmware
)

command -v cargo-readme > /dev/null || cargo install cargo-readme

render() { cargo readme --project-root "$1" --no-indent-headings; }

mode=render
case "${1:-}" in
  --list)
    printf '%s\n' "${CRATES[@]}"
    exit 0
    ;;
  --check)
    mode=check
    shift
    ;;
esac

targets=("${@}")
((${#targets[@]})) || targets=("${CRATES[@]}")

status=0
for crate in "${targets[@]}"; do
  if [[ "$mode" == check ]]; then
    if ! diff -u "$crate/README.md" <(render "$crate") >&2; then
      echo "$crate/README.md is stale — run scripts/gen-readme.sh (the pre-commit hook does)" >&2
      status=1
    fi
  else
    render "$crate" > "$crate/README.md"
  fi
done
exit "$status"
