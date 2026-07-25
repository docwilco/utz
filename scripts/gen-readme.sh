#!/usr/bin/env bash
# README.md = README.tpl with {{readme}} replaced by the utz crate docs
# (the leading //! block of utz/src/lib.rs), rustdoc-only syntax adapted
# for GitHub markdown:
#   - intra-doc links keep their text; http(s) links survive
#   - `ignore`/`no_run` code fences become plain ```rust
# `--check` compares instead of writing (CI mode).
set -euo pipefail
cd "$(dirname "$0")/.."

crate_docs() {
  awk '!/^\/\/!/ { exit } { sub(/^\/\/! ?/, ""); print }' utz/src/lib.rs |
    perl -pe '
      s/\[([^\]]+)\]\((?!https?:)[^)]*\)/$1/g;
      s/^```(ignore|no_run|rust[^`]*)$/```rust/;
      s/`μTZ`/μTZ/g;
    '
}

render() {
  sed '/{{readme}}/,$d' README.tpl
  crate_docs
  sed '1,/{{readme}}/d' README.tpl
}

if [[ "${1:-}" == "--check" ]]; then
  if ! diff -u README.md <(render) >&2; then
    echo "README.md is stale — run scripts/gen-readme.sh (the pre-commit hook does)" >&2
    exit 1
  fi
else
  render > README.md
fi
