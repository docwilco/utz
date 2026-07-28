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

# every utz* directory in the workspace root is a crate
CRATES=()
for d in utz*/; do
  [[ -f "$d/Cargo.toml" ]] && CRATES+=("${d%/}")
done

command -v cargo-readme > /dev/null || cargo install cargo-readme

# Crate docs may link to sibling crates with doc-root-relative paths
# (`../utz_build/struct.Config.html`) so rustdoc output works in any
# shared doc root; a README has no doc root, so rewrite those links to
# the hosted docs.
DOCS_URL="https://docwilco.github.io/utz/docs/"
render() {
  cargo readme --project-root "$1" --no-indent-headings \
    | sed -e "s|]: \.\./|]: ${DOCS_URL}|g" -e "s|](\.\./|](${DOCS_URL}|g"
}

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
