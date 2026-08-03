# Releasing μTZ to crates.io

## Before the first publish

- [ ] Verify the crate names are free (or already yours) on crates.io:
      `utz`, `utz-common`, `utz-simplify`, `utz-encode`, `utz-build`,
      `utz-build-cli`, `utz-data-tiny`, `utz-data-tiny-static`,
      `utz-data-compact`, `utz-data-balanced`, `utz-data-accurate`.
- [ ] `cargo login` with a token that has publish scope.
- [ ] Fresh assets: `./scripts/gen-presets.sh` (the data crates' build.rs
      recipe guards refuse mismatched assets, so a stale asset cannot
      slip through, but generate before packaging so the tarballs carry
      the current TZBB release).
- [ ] `./scripts/checks.sh all` green.

## Version policy

Versions carry the TZBB release as semver *build metadata*
(`0.2.0+2026c`). Cargo and crates.io ignore build metadata for ordering
and equality: `0.1.0+2026c` and `0.1.0+2027a` are the SAME version, and
crates.io will reject the second as a duplicate. Therefore:

- **Every TZBB data refresh bumps the real version** (patch for a pure
  data refresh, minor/major per semver for API changes) and updates the
  `+tag` to match. The tag is informational; the number does the work.
- The recorded release is also queryable at runtime
  (`Finder::tzbb_release()`) and stamped in every asset header.

## Publish order

Internal dependencies force this order; wait for the registry to serve
each crate (usually well under a minute) before publishing its
dependents. `cargo publish` runs a verify build resolving deps from the
registry, which is also why only `utz-common` and `utz-simplify` can be
package-verified before anything is published (CI's `package` job does
exactly those two).

```sh
cargo publish -p utz-common
cargo publish -p utz-simplify
cargo publish -p utz-encode
cargo publish -p utz-data-tiny
cargo publish -p utz-data-tiny-static
cargo publish -p utz-data-compact
cargo publish -p utz-data-balanced
cargo publish -p utz-data-accurate
cargo publish -p utz
cargo publish -p utz-build
cargo publish -p utz-build-cli
```

Notes per crate:

- **utz-data-***: the `.utz` assets are gitignored but force-included by
  each crate's `include` list; `cargo package --list -p utz-data-tiny`
  should show `data/tiny.utz`, `build.rs`, `LICENSE`, `LICENSE-DATA`.
  These crates are `license = "MIT AND ODbL-1.0"`: the packaged asset is
  a derivative database of timezone-boundary-builder (OpenStreetMap,
  ODbL); LICENSE-DATA carries the attribution.
- **utz-build**: consumers' build scripts download sources at build time
  (never on docs.rs, which has no network — the crate docs tell users
  to vendor via `UTZ_CACHE_DIR` / `Config::cache_dir()` for hermetic
  builds).
- **utz-build-cli**: after publishing, `cargo install utz-build-cli`
  is the supported way to get `gen`/`gen-preset` outside a checkout.

## Never published

`utz-dev-cli`, `utz-viz`, `utz-bench-common`, `utz-bench-cli`, and
`utz-bench-firmware` are repo tooling, `publish = false` on purpose. The
deployed viewer (github.io) is built by `utz-dev-cli visualize` from a
checkout.

## After publishing

- [ ] Tag the release: `git tag v<utz-version> && git push --tags`.
- [ ] Check the docs.rs builds (utz builds with the feature set pinned
      in `[package.metadata.docs.rs]`; presets are deliberately excluded
      there because the assets are not in utz's package).
- [ ] Known cosmetic issue: cross-crate links between the μTZ crates 404
      on docs.rs (they assume the combined doc root); the canonical docs
      remain https://docwilco.github.io/utz/docs/.
