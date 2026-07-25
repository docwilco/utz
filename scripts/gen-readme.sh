#!/usr/bin/env bash
# utz/README.md = utz/README.tpl + the utz crate docs (the //! block of
# utz/src/lib.rs), rendered by cargo-readme. `--check` compares instead
# of writing (CI mode).
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo-readme > /dev/null || cargo install cargo-readme

render() { cargo readme --project-root utz --no-indent-headings; }

if [[ "${1:-}" == "--check" ]]; then
  if ! diff -u utz/README.md <(render) >&2; then
    echo "utz/README.md is stale — run scripts/gen-readme.sh (the pre-commit hook does)" >&2
    exit 1
  fi
else
  render > utz/README.md
fi
