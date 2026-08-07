# Releasing μTZ to crates.io

## Before the first publish

- [ ] Verify the crate names are free (or already yours) on crates.io:
      `utz`, `utz_common`, `utz_simplify`, `utz_encode`, `utz_build`,
      `utz_build_cli`, `utz_data_tiny`, `utz_data_tiny_static`,
      `utz_data_compact`, `utz_data_balanced`, `utz_data_accurate`.
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
registry, which is also why only `utz_common` and `utz_simplify` can be
package-verified before anything is published (CI's `package` job does
exactly those two, in a single `cargo package` invocation: utz_simplify's
dev-dependency on utz_common resolves against the sibling there).

`utz` publishes with `--no-verify`: its deliberate compile-error guards
(pick an asset source / environment / geometry decoder) fire on the
zero-feature build the verify step runs. The feature matrix is fully
built by `./scripts/checks.sh all` beforehand, on the same sources the
package contains. The data crates publish with `--allow-dirty`: their
gitignored-but-included `.utz` assets trip cargo's dirty check.

`./scripts/publish.sh` runs the whole sequence: it publishes in the
order below, retries through crates.io's new-crate rate limit, waits
for the registry to serve each crate before its dependents, and skips
already-published crates, so a partial run can be rerun from the top.

```sh
cargo publish -p utz_common
cargo publish -p utz_simplify
cargo publish -p utz_encode
cargo publish -p utz_data_tiny --allow-dirty
cargo publish -p utz_data_tiny_static --allow-dirty
cargo publish -p utz_data_compact --allow-dirty
cargo publish -p utz_data_balanced --allow-dirty
cargo publish -p utz_data_accurate --allow-dirty
cargo publish -p utz --no-verify
cargo publish -p utz_build
cargo publish -p utz_build_cli
```

Notes per crate:

- **utz_data_***: the `.utz` assets are gitignored but force-included by
  each crate's `include` list; `cargo package --list -p utz_data_tiny`
  should show `data/tiny.utz`, `build.rs`, `src/lib.rs`, `README.md`,
  `LICENSE`, and `LICENSE-DATA` (plus cargo's own metadata files).
  These crates are `license = "MIT AND ODbL-1.0"`: the packaged asset is
  a derivative database of timezone-boundary-builder (OpenStreetMap,
  ODbL); LICENSE-DATA carries the attribution.
- **utz_build**: consumers' build scripts download sources at build time
  (never on docs.rs, which has no network — the crate docs tell users
  to vendor via `UTZ_CACHE_DIR` / `Config::cache_dir()` for hermetic
  builds).
- **utz_data_accurate**: ~8.1 MB of already-compressed asset, ~81% of
  crates.io's default 10 MiB upload limit. Watch the size on every TZBB
  refresh and request a per-crate limit raise from the crates.io team
  before it crosses.
- **utz_build_cli**: after publishing, `cargo install utz_build_cli`
  is the supported way to get `gen`/`gen-preset` outside a checkout.

## Never published

`utz_dev_cli`, `utz_viz`, `utz_bench_common`, `utz_bench_cli`, and
`utz_bench_firmware` are repo tooling, `publish = false` on purpose. The
deployed viewer (github.io) is built by `utz_dev_cli visualize` from a
checkout.

## After publishing

- [ ] Tag the release: `git tag v<utz-version> && git push --tags`.
- [ ] Check the docs.rs builds (utz builds with the feature set pinned
      in `[package.metadata.docs.rs]`; presets are deliberately excluded
      there because the assets are not in utz's package).
- [ ] Cross-crate doc links resolve everywhere: local and Pages builds
      use the shared doc root, while docs.rs builds get absolute docs.rs
      URLs via the `on_docsrs` cfg that only the crates' docs.rs metadata
      sets (first markdown link definition wins). The canonical docs
      remain https://docwilco.github.io/utz/docs/.
