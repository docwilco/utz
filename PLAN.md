# μTZ — micro-timezone

A tiny, embeddable latitude/longitude → IANA timezone lookup crate. Ground-up
rewrite; keeps only the *contract* of the old `spatialtime` (Reader-style API,
OSM source), replaces the whole engine.

Working crate name: **`utz`** (project: μTZ / micro-timezone).

---

## 1. Goals

- **Tiny.** OSM timezone data down to ~125–460 KB (vs tzf-rs ~5–7 MB) via
  shared-arc topology + tunable line simplification (Ramer–Douglas–Peucker,
  "RDP", today; §14 for alternatives) + integer quantization + general compression.
- **Embeddable / `no_std`-friendly.** Pure-Rust codecs, integer PIP, flat arrays
  that borrow zero-copy from a flash partition. Targets ESP32/Xtensa-class.
- **Tunable at build time.** Dataset, simplification ε, quant grid, grid cell size, codec —
  build exactly the size/RAM/accuracy point you need, guided by a viz tool.
- **DST-correct.** Returns the IANA `tzid`; resolve offsets/DST downstream
  (`jiff` — its `jiff-static` compile-time zones suit embedded; or the prevalent
  `chrono-tz`; `time` lacks IANA tz support without third-party crates).
  `-1970` option gives tzids valid for past timestamps too
  (back to 1970), not just current/future time.

Non-goals: shipping committed assets; NED dataset (dropped — RDP+topology on OSM
is ≤ NED size at real fidelity); being the batch-throughput champion (tzf/rtz
exist for server use).

---

## 2. Workspace layout (flat, no `crates/` subdir)

```
utz/                 workspace root
  Cargo.toml         [workspace] members = ["utz", "utz-build"]
  PLAN.md            this file
  .gitignore         cache/, assets/, *.geojson, *.utz, viewers

  utz/               runtime library crate — NO build.rs (§11: data ships as
    src/             preset data crates; custom assets are consumer-generated)
      lib.rs         public API, feature gates, grouped compile_errors
      decompress.rs  codec backends (uncompressed/gzip/zstd/brotli/xz)
      format.rs      self-describing header, zone table, arc store, ring index
      grid.rs        ndarray cell → zone-id | spill-index (+ CSR spillover)
      pip.rs         hand-rolled per-polygon integer PIP (i64/i128)
      finder.rs      Finder: new()/from_static()/from_reader() + lookup()/lookup_coarse()

  utz-simplify/      open-polyline simplification menu (RDP / Visvalingam–Whyatt /
    src/             Imai–Iri), shared by utz-build and — as a wasm32 cdylib —
      lib.rs         the tuning HTML (§14.8); wasm.rs = raw extern "C" surface
      wasm.rs

  utz-data-*/        preset data crates (§11): nano/micro/balanced/accurate —
                     generated + published by CI per TZBB release, not committed

  utz-build/         consumer build-dependency (builder API) + CLI (`gen`)
    src/             + dev/exploration + viz tool
      lib.rs         re-exports encoder + measurement helpers
      types.rs       Feat/Ring/Poly, quantization helpers
      loader.rs      source → Vec<Feat>  (geojson; fgb reader kept for now)
      topo.rs        shared-arc topology + topology-aware per-arc simplification
      grid.rs        grid + interned-CSR builder
      encode.rs      container serializer (header + sections + compress)
      download.rs    conditional GET (ETag / Last-Modified)
      viz.rs         emit the tuning HTML
    examples/        measurement binaries (bench, sweeps) — continue here
```

`utz-build` is where the exploration/measurement continues (the `formatlab`
prototypes get ported here). It is a build-dependency of *consumers* (custom tier,
§11), the CLI, the generator behind the `utz-data-*` crates, and the home of the
viz tool and benchmarks. `utz` itself never depends on it.

---

## 3. Public API

Self-describing format ⇒ one `Finder` type, any variant, multiple sources:

```rust
impl Finder {
    fn new() -> Result<Finder>;                    // preset asset (exactly one data feature on, §11)
    fn from_static(bytes: &'static [u8]) -> Result<Finder>;  // flash partition: ZERO-COPY (uncompressed)
    fn from_reader(r: impl Read) -> Result<Finder>;          // file / network / OTA: owned buffer

    fn lookup(&self, lon: f64, lat: f64) -> Option<&str>;    // accurate: grid → PIP
    fn lookup_coarse(&self, lon: f64, lat: f64) -> Option<&str>; // grid-only, no geometry, ~cell-size error
}
```

- **`from_static`** is the embedded/flash win — borrows the bytes, no RAM copy in
  `uncompressed` mode. `impl Read` can't zero-copy, so it's the std/OTA path.
- **Availability by environment rung (§11):** `core` = `from_static` +
  `lookup_coarse`; `alloc` adds `lookup` + `from_vec`; `std` adds `from_reader`.
- **No-embed deployment** (= no preset feature enabled): ship the binary *without*
  the asset, load it at runtime from a flash partition (`from_static`) or file
  (`from_reader`); generate the asset with the `utz-build` CLI (§11). Enables
  **OTA-updatable tz data** (swap `-now`↔`-1970`, new TZBB vintage) without
  reflashing firmware.
- **`lookup_coarse`** (learned from tzf's FuzzyFinder): answer from the grid alone — no arcs
  loaded, ~cell-size border error, tiny + instant. Optional mode.
- **Return `Option<&str>`** (tzid, borrowed from the zone table). DST resolved
  downstream. `None` only if truly uncovered (with-oceans has full coverage).
- API naming: **Finder / lookup** chosen. `(lon, lat)` order (x, y) — document
  loudly; consider a `LonLat` newtype to kill the ordering footgun. (Open.)

Kept from old spatialtime: the `new()`/`lookup()` *shape*, the OSM source URLs,
and the known-point tests as regression fixtures. Nothing else survives.

---

## 4. On-disk / embedded format (self-describing)

```
header:      magic, version, dataset(now|1970|all), tzbb_release,
             rdp_eps, quant_bits, grid_deg, codec
zone table:  tzid string pool + offsets
arc store:   per arc: [varint vcount][i{16,24,32} first vertex][zigzag-varint deltas]
ring index:  feature → polygon → ring = signed arc refs
grid:        Array2<u16> primary (tagged) + interned-CSR (list_offsets u16, list_ids u16)
```

The header records **every knob**, so the runtime decoder is **generic** — changing
ε/quant/grid regenerates the asset but never the decode code. This keeps the
feature matrix small and lets one binary read any variant handed to it.

**Arc topology (our Format B):** shared borders are cut into arcs at junctions,
each arc stored once, rings are lists of signed arc refs. Removes the ~43–74%
duplicated shared-border coordinates at the format level (measured NED 74% shared
edges; validated by tzf/ZoneDetect-v1 independently converging on the same design).

---

## 5. Build pipeline (runs in `utz-build`: consumer build.rs, CLI, or data-crate CI — §11)

1. **Download** `timezones-with-oceans[-now|-1970].geojson.zip` (no suffix =
   `all`) → `cache/`.
   **Conditional GET**: store `ETag`/`Last-Modified`, send
   `If-None-Match`/`If-Modified-Since` → 304 reuses cache. Record TZBB release in
   the header (DST vintage + cache-invalidation).
2. **Parse** → features (tzid + MultiPolygon).
3. **Topology**: dedup vertices, cut shared arcs at junctions.
4. **Topology-aware RDP** at ε (each arc simplified once → borders stay stitched).
5. **Quantize** arcs (i16/i24/i32), delta + zigzag-varint.
6. **Grid** at cell size: rasterize borders → `Array2<u16>` (zone-id | spill-index)
   + interned-CSR spillover.
7. **Serialize** self-describing container → **compress** (chosen codec) → the
   caller's sink: consumer `$OUT_DIR/tz.utz` (build.rs → `include_bytes!`), a
   file (CLI `-o`), or a data crate's static (CI).
8. **Two-level cache**: cache the built artifact keyed by hash(TZBB release + all
   knobs), so unchanged rebuilds are instant.
- **docs.rs / hermetic**: `utz` itself has no build.rs, so presets are trivially
  fine; custom consumer build.rs can pass a pre-fetched source zip (URL/path knob)
  where downloads are forbidden.
- **Cost note**: first build is heavy (47 MB zip, 156 MB json, topology over
  millions of verts + q11 compression). The two caches make it one-time.

---

## 6. Datasets

- **`-now`** (65 zones): merges currently-clock-identical zones → smaller,
  representative tzids (e.g. Amsterdam→`Europe/Paris`).
- **`-1970`** (304 zones): merges only zones identical since 1970 → IANA's own
  canonical equivalence (`zone1970.tab`); the tzid's rule history **matches the
  location for any timestamp back to 1970**, so past conversions are right too.
  Bigger than `-now`.
- **`all`** (444 zones, no URL suffix; TZBB calls it "Comprehensive", parser also
  accepts `full`): no merging at all — one polygon per `zone.tab` tzid, keeping
  pure aliases distinct (`Europe/Oslo` ≠ `Europe/Berlin`). **Unique per-country
  tzid string** for display/interop. Caveat: most zones → most data → largest
  asset and heaviest build; clock behavior gains nothing over `-1970`.

Exactly one selected. `-now` default (smallest); document the tzid-representative
caveat so users pick `-1970` when past timestamps must convert correctly, or
`all` when the unique country-level tzid string itself is the product.

**Source URLs** (timezone-boundary-builder, `releases/latest/download/`, GeoJSON zip).
Six variants: {land-only, with-oceans} × {all, -1970, -now}. μTZ uses the
**with-oceans** ones (global coverage — land-only leaves the sea uncovered):

```
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-1970.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-now.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans.geojson.zip        # μTZ `all` (unmerged, 444 zones)
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans-1970.geojson.zip   # μTZ `1970`
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans-now.geojson.zip    # μTZ `now` (default)
```

(`all` grid/size numbers not yet measured — §10 table covers `-now`/`-1970`;
extend the sweep when the `all` knob lands.)

---

## 7. Compression

Codecs (decoder feature-gated, exactly one): **uncompressed, gzip
(miniz_oxide, pure-Rust), zstd (`zstd-sys` C + `ruzstd` pure-Rust), brotli, xz**
(`lzma-rust2`). Everything but `zstd-sys` is pure Rust (Xtensa-friendly). Window/
dict tiers as before. `uncompressed` enables zero-copy `from_static`.

---

## 8. Quantization & PIP

**Store quantized (i16/i24), compute in a wider int.** Findings:
- `geo` does integer PIP (`Contains` needs `GeoNum`; impl'd i16/i32/i64/i128) but
  `orient2d` computes in `T` with **no widening** → i32/i16 overflow (i32 was
  **94.6% wrong**). **geo-i64 == geo-f64, 0/8000 disagreements**, incl. ocean/pole
  cases our hand-rolled even-odd got wrong.
- **Overflow bound: product ≤ 4·coord_max².** i24 (±8.4e6) → i64 safe (≤2.8e14).
  i32/deg×1e7 (±1.8e9) → **overflows i64** (1.3e19) → needs **i128**.

**Decision: hand-rolled per-polygon integer PIP, width follows the quant grid.**
- i16/i24 storage → **i64** compute. i32-fine storage → **i128**. Chosen from the
  header's `quant_bits` (i64 default, i128 fallback).
- **Per-polygon** (exterior parity, minus holes) — NOT the all-rings even-odd that
  caused the old 3.8% ocean/pole bug. Grid-driven: usually 0–2 polygons decoded.
- **Why i64 over f64:** exact on gridded data (no rounding to be robust against),
  **deterministic** across platforms/compilers (integer math), faster (SimpleKernel-
  style predicate), fewer deps. f64 only re-introduces rounding we quantized away.
- **`geo` as dev-dependency only** — cross-validate the hand-rolled PIP in tests
  (geo-i64 is a proven oracle). Keeps `geo`/std out of the runtime → `no_std` clean.
- **Antimeridian**: geo handles the shipped data correctly (planar), suggesting
  TZBB with-oceans is antimeridian-split. **Verify** (open item); if any polygon
  truly crosses ±180, add a build-time split pass.

---

## 9. Rings / memory strategy

Decision: **both eager and lazy — API-selected, not feature-selected** (availability
falls out of the §11 environment rungs: both need `alloc`), plus `lookup_coarse`
(the `core` floor).
- **Lazy** (`lazy`): grid → candidate ids → decode **one polygon at a time** from
  the arc store (resolve arc refs → i64), PIP, discard. Working set = largest single
  candidate polygon. Interior cells decode **zero** geometry. Best for embedded.
- **Eager** (`eager`): decode all polygons into RAM in `new()`. Fastest repeat
  lookups, highest RAM. Server/std.
- **Coarse** (`lookup_coarse`): grid-only, no arcs. ~cell-size error, ~KBs, no geometry loaded.
- **Per-ring streaming** (even less RAM than per-polygon) is a possible embedded
  fast-path for a rare huge polygon; needs the hand-rolled PIP (fine, we own it).
  Defer unless a single simplified polygon is large enough to matter.

---

## 10. Grid (ndarray + interned CSR)

- **`Array2<u16>` primary**, one tagged `u16` per cell:
  - high bit 0 → interior cell → low 15 bits = **zone id** (O(1), no PIP).
  - high bit 1 → border cell → low 15 bits = **index into interned candidate lists**.
- **Interned-CSR spillover** (a dictionary of unique candidate-lists; coastlines
  repeat `{land, ocean}`): `list_offsets: [u16; uniq+1]`, `list_ids: [u16; uniq_ids]`.
- **All fixed-width u16 → identical on 32/64-bit**, and the whole grid can live in
  `&'static` flash (`ArrayView2::from_shape` over a borrowed slice). The `u16`-per-
  cell primary is the irreducible floor; the side table is tiny after interning.
- **Cell size: integer degrees only, presets {1, 2, 3, 5, 10}.** Fractions **not
  worth it** — primary ∝ 1/d²: 0.5° = ~518 KB, 0.1° = ~13 MB. Default **2° or 3°**.

**Measured (2°):**
| dataset | cells | border | unique lists | interned total |
|---|--:|--:|--:|--:|
| `-now` (65) | 16,200 | 3,960 | 300 | **33.7 KB** |
| `-1970` (304) | 16,200 | 4,838 | 1,163 | **40.0 KB** |

**P(PIP)** (area-uniform lookups needing PIP; rest are O(1) interior):
1°=15/18%, 2°=28/33%, 3°=39/45%, 5°=56/63%, 10°=85/88% (`-now`/`-1970`).

- **Dominant-first ordering** (put the largest-area zone first in each candidate
  list for PIP early-exit): free in id bytes, **but breaks interning** (`[A,B]` vs
  `[B,A]` stop deduping) → more unique lists. **Cost unmeasured — measurement item.**
- **Overlapping/`(start,len)` spillover** (extent table / tail-merging) can shrink
  the id pool further, but at 2° the pool is ~1 KB next to the 32 KB primary — skip
  until a finer grid / the unmerged `all` dataset makes the pool the bottleneck.

---

## 11. Features & config — **decided**: preset data crates + consumer-side custom

Two **mandatory, at-least-one-of feature choices**. (Unification-safe by
construction: an "at least one of N" error can only be *silenced* by feature
union, never triggered — unlike "exactly one of N", which union breaks.)

1. **Data tier:** `nano` / `micro` / `balanced` / `accurate` (prebuilt data
   crates) or `custom` (consumer generates the asset).
2. **Environment:** `std` / `alloc` / `core` (ladder, see below).

`default = []` — forgetting to choose fails loudly, with the error message as
onboarding (embedded-friendlier than the ecosystem's silent `default = ["std"]`,
where a forgotten `default-features = false` drags `std` into firmware):
```rust
compile_error!("utz: pick a data tier: a preset (`nano`/`micro`/`balanced`/`accurate`) \
                or `custom` (bring your own asset, generated with utz-build)");
compile_error!("utz: choose an environment: `std`, `alloc` (no_std + allocator), \
                or `core` (bare metal: coarse lookup, uncompressed assets only)");
```
The forcing function is per-tree, not per-consumer (any dependency's choice
unifies in) — accepted. docs.rs builds with a representative set via
`[package.metadata.docs.rs]`.

**Environment ladder** — `std = ["alloc"]`, each rung a strict superset, so a
feature union resolves upward. `core` gates nothing extra (marker) but states
deliberate bare-metal intent and satisfies choice 2:

| rung | constructors | lookups | codecs |
|---|---|---|---|
| `core` | `from_static` (zero-copy) | `lookup_coarse` | uncompressed only |
| `alloc` | + `from_vec` | + `lookup` (PIP spill/scratch `Vec`) | + `gzip`/`ruzstd` |
| `std` | + `from_reader` | — | + `brotli`/`xz`/`zstd-sys` (as gated today) |

- **Memory-mode features dissolved** (`eager`/`lazy`/`coarse` are no longer
  features): coarse is what `core` can do, lazy is `lookup` under `alloc`, eager
  is a constructor option under `alloc`. Availability falls out of the rung —
  API surface, not features (§9).
- **`core` pairs naturally with `custom`:** CLI-generate an **uncompressed**
  asset to a flash partition. The CLI grows `--coarse-only`: strips the arc
  store, header marks "no geometry section" (`lookup` → runtime `Err`), asset
  shrinks to grid + zone table — a coarse-only device pays for exactly what it
  uses. Asset-shape → builder/CLI knob, not a feature.
- Door open (§14.7): alloc-free accurate lookup (fixed/caller-provided scratch)
  would later promote `lookup` to the `core` rung — no feature reshuffle needed.

**The tiers:**

- **Presets (features → data crates):** `utz-data-nano` … `utz-data-accurate`,
  each containing one CI-generated `.utz` as a static. On `utz`, feature `nano` =
  `["dep:utz-data-nano", "alloc", <its codec feature>]` — preset assets are
  compressed, so presets imply `alloc` and their codec must be no_std-clean
  (`gzip`/`ruzstd`, not `brotli`/`xz`/`zstd-sys`) — constraint on §14.5.
  Consumer: `utz = { features = ["std", "balanced"] }` → `Finder::new()`.
  Presets bake dataset `now`; other datasets are custom (or later preset
  variants — §14.5).
- **Custom (the fifth tier):** a marker feature — gates nothing
  (`from_static`/`from_reader` stay available to everyone; preset users want
  them for OTA), it states intent and satisfies choice 1. Generate the bytes with:
  - *consumer `build.rs`* (`prost-build` pattern): `utz-build` as a
    build-dependency; typed builder API **is** the config — rustdoc'd,
    IDE-completable, no file discovery (`CARGO_MANIFEST_DIR`/`OUT_DIR` are the
    consumer's own): `utz_build::Config::new().dataset(Now).rdp_meters(500.0)
    .generate()?` → `include_bytes!(concat!(env!("OUT_DIR"), "/tz.utz"))`.
  - *CLI* (`icu_datagen` pattern): `utz-build gen --rdp 500 -o tz.utz` — for
    flash-partition/OTA images, experiments, and the CI that builds the data
    crates. Assets are **never committed to a repo**; they're regenerated
    (downloads are cond-GET-cached, so regeneration is cheap).
- **Remaining `utz` features are purely code-shape and additive:** the codec
  decoders (as today). Everything else is API whose availability falls out of
  the environment rung.

**Why this shape (over features-for-knobs, env vars, or a discovered `utz.toml`):**
- **Additivity solved, not fought.** Data crates are statics; two crates in the
  tree enabling different presets both link, the unreferenced one is dead-stripped.
  No one-of-N `compile_error!` boilerplate anywhere. `Finder::new()` exists only
  when *exactly one* preset feature is on (cfg'd out otherwise — use
  `from_static(utz::data::NANO)` explicitly); an asset whose codec byte has no
  compiled decoder is a runtime `Err`, not a compile error (self-describing header).
- **Hermetic where it matters.** The old plan had `utz/build.rs` downloading TZBB
  in *every consumer's* build — broken on docs.rs, Nix, Bazel, Debian, offline CI.
  Now the presets (the common path) are plain bytes from crates.io. The custom
  tier *does* download TZBB — deliberately, since source data is fetched, never
  committed — but it's opt-in, cond-GET-cached, and hermetic consumers can point
  `Config` at a pre-fetched source zip (URL/path knob).
- **No ambient config.** Env/`[env]`-based schemes (incl. the earlier
  `UTZ_CONFIG` + `relative = true` design) hinge on cargo *finding*
  `.cargo/config.toml` by cwd walk-up — `--manifest-path` / IDE / multi-checkout
  CI invocations silently miss it and build with default knobs. Rejected for that
  silent-misconfiguration mode; a builder API or committed asset can't be missed.
- **Costs accepted:** data crates republished per TZBB release (CI-automated;
  ≤ ~500 KB each, well under crates.io limits); preset+tweak means going custom
  (three lines of build.rs).
- **Provenance note:** the `.utz` in a data crate is gitignored and published via
  `cargo publish --allow-dirty`, so the artifact isn't byte-reproducible from a
  git checkout alone. Reproducibility comes from the self-describing header
  (TZBB release + all knobs → regenerate and diff); the CI publish job should
  also attach generation logs + checksums to a GitHub release.

**Prior art:** `prost-build`/`slint-build`/`tonic-build` (consumer build.rs,
builder-API-as-config), `icu_datagen`/databake + `chrono-tz` (pregenerated /
data-in-crate), `getrandom` (why one-of-N features fail: additivity).

---

## 12. Visualization

`utz-build/viz.rs` + `cargo run -p utz-build --example visualize` regenerates the
viewers (keyless Carto/Esri tiles, scale bar, on-the-fly JS quantization; HTML
self-embeds data → generated artifact, not a committed asset). Users tune
ε/quant/grid **before** committing the build knobs. Link a CI-built copy from docs.
- **overlay**: precomputed RDP ε levels × quant grids, reduction-stats panel
  (stored arc verts vs ε=0, raw coord bytes at the chosen width).
- **live**: full-res arcs + `utz-simplify` compiled to WASM — three independent
  "sets" (algorithm/ε/quant dropdowns, per-set color), raw-f64 overlay toggle,
  per-set reduction stats. Each set computes in a Web Worker (spinner, UI stays
  live); changing settings terminates the worker and recomputes fresh.
- **border**: Portugal/Spain detail sweep for visual fidelity checks.

---

## 13. How μTZ differs from tzf-rs (why build it)

Win: **~10× smaller** (general compression tzf lacks + tunable aggressive RDP + int
quant): ~125–460 KB vs tzf ~5–7 MB. **Genuinely `no_std`/flash-embeddable** (tzf is
std/protobuf, can't zero-copy from flash, can't run on ESP32). **Tunable** to an
exact size/RAM/accuracy point. **`-1970`** for tzids valid back to 1970 (past
timestamps convert right), **`all`** for the unique per-country tzid string.
Not-better: tzf is mature/tested; we reuse its good ideas (topology, 1° grid,
delta-varint); if you don't need embedded/tiny, tzf already exists.

**Learned from tzf (adopt now):** grid-only coarse mode (tzf calls it "fuzzy"); ship a balanced preset;
embed TZBB version; verify antimeridian handling. **Defer:** hierarchical/quadtree
grid (1°-accuracy at coarse memory); per-polygon YStripe edge index (faster PIP on
big polygons — note tzf's `geometry-rs` Rust port dropped the Go original's index,
its `contains_point` is a plain linear ring walk). ~~benchmark `geo` vs
`geometry-rs`~~ — done, see §15 (3-way `pip_bench`).

---

## 14. Open decisions (continue later)

1. ~~**Build-knob mechanism**~~ — **decided** (§11): preset data crates
   (`nano`/`micro`/`balanced`/`accurate` features) + consumer-side custom
   generation via the `utz-build` builder API / CLI. Supersedes the earlier
   `utz.toml`/`UTZ_CONFIG` `[env]` design (rejected: silent cwd-discovery
   failure, non-hermetic build.rs downloads in every consumer).
2. ~~**`geo` vs hand-rolled PIP**~~ — **decided**: hand-rolled i64 (`utz/src/pip.rs`),
   geo dev-oracle only. 0/20k disagreements, speed parity with geo after
   adopting its loop shape (see §15).
3. **`LonLat` newtype** vs raw `(lon, lat)` to prevent order footgun.
4. ~~**Antimeridian**~~ — **verified pre-split** (see §15); no split pass.
5. **Preset values** (ε/quant/grid/codec) for `nano`/`micro`/`balanced`/`accurate`
   (§11); whether non-`now` datasets get preset variants (e.g. `balanced-1970`)
   or stay custom-only.
6. Crate/repo name confirmed `utz`; public naming of feature groups.
7. **Alloc-free *accurate* lookup** (discuss): the alloc-free *coarse* floor now
   ships as §11's `core` rung (`from_static` + `lookup_coarse`, uncompressed).
   Still open: the accurate path without heap — caller-provided or fixed-size
   scratch buffer (bound = largest decoded polygon, a build-time-known number
   that could go in the header). Worth it for heapless targets / ISR-context
   lookups? Costs: API surface (buffer-passing or const-generic capacity), a
   header field. Would promote `lookup` from `alloc` to `core` (§11). Decide
   after a real embedded consumer.
8. ~~**Simplification algorithm menu**~~ — **decided + built**: the
   `utz-simplify` crate (workspace member, `lib` + `cdylib`) holds the
   open-polyline menu, shared by the builder (`topo::build_topology_algo`,
   RDP default via `Simplify` enum) and — compiled to wasm32-unknown-unknown,
   ~33 KB — the tuning HTML, so the browser preview runs the exact builder
   code (raw `extern "C"` surface in `utz-simplify/src/wasm.rs`, no
   wasm-bindgen). Menu:
   - **Ramer–Douglas–Peucker** (`rdp`): max deviation ≤ ε; the default.
     (Port fix: distance is now to the *segment* (clamped projection), the
     old inline version measured to the infinite line — keeps a few more
     points, actually honors the ε bound.)
   - **Visvalingam–Whyatt** (`visvalingam`): smallest-triangle removal, area
     knob (no ε-equivalence faked), deterministic tie-break on index.
   - **Imai–Iri** (`imai_iri`): provably minimum vertices within ε — BFS over
     the shortcut graph with **Chan–Chin wedge validity** (per anchor, sweep
     targets keeping the intersection of "ray passes within ε of point k"
     angular wedges; segment valid ⟺ forward wedge at i ∧ backward wedge at j;
     O(1) amortized per check, exact — matches the naive-scan oracle at
     n=3000, brute-force-verified optimal on small inputs). Watch the arc
     wraparound: interval intersection must use *membership* tests, pairwise
     cross-sign comparison silently accepts disjoint intervals > 180° away.
     Arcs > 8192 pts prefilter with `rdp(ε/10)`, escalating toward ε/2 only
     while still too big — bounds compose, total ≤ ε, near-optimal. Measured
     vs RDP on full-planet `-now` arcs: **−3.8% verts at ε=100 m, −18% at
     500 m, −19% at 2000 m**, ~1 s for all 1372 arcs (WASM) — strong
     candidate for the default algorithm.
   - Corridor/streaming family (Reumann–Witkam, Opheim, Lang, Zhao–Saalfeld):
     **rejected** — quality-per-vertex worse than RDP; their single-pass
     speed advantage is worthless at build time.
   Still open: `simplify_algo` header byte + its builder-API/CLI knob (§11) for
   selecting VW/II per asset; size-vs-RDP sweep for
   Imai–Iri on real arcs to see if it should become the default.

---

## 15. Measurement backlog (do in this workspace)

- [x] **Dominant-first interning cost** — measured (`dominant_cost` example) at 2°:
  per-cell dominant-first costs **+1.3 KB** `-now` (300→486 lists) / **+3.1 KB**
  `-1970` (1163→1616), but lifts P(first-PIP-hit) 53%→**78.8%** / 46%→**78.2%**.
  Global-area-desc ordering is free (interning preserved by construction) but only
  helps `-1970` (46→53%). Verdict: dominant-first worth it — KBs are noise vs the
  32 KB primary; halves expected PIP work on border cells.
- [x] **Hand-rolled i64 PIP vs `geo` vs `geometry-rs`** — done (`utz/src/pip.rs` +
  `pip_bench`, 3-way): **0/20,000 disagreements** on quantized OSM ε=500 m, both
  datasets (incl. geometry-rs, whose boundary semantics differ but never off
  boundary). Speed with equal hoisted bbox prechecks: **even with geo**
  (1.00–1.04×), **1.25–1.27× faster than geometry-rs** — after adopting geo's
  loop shape (one cross product per scanline-touching edge decides crossing AND
  boundary-collinear; the old loop ran vertex/horizontal boundary branches on
  every edge, costing ~35%). (An earlier "**14.5×/51× ours faster**" figure was
  a bench bug: geo 0.32 `Polygon::contains` has NO internal bounding-rect
  precheck — only ours got a bbox test, so geo walked every ring in the scan.
  Corrected 2026-07.) Decision rests on non-speed grounds anyway: `no_std` with zero deps, zero-copy
  `&[(i32,i32)]` slices straight from the arc decoder (geo/geometry-rs need
  owned i64/f64 ring `Vec`s — per-lookup allocs in lazy mode), deterministic
  boundary-claimed semantics, i128 variant for i32 grids (geometry-rs's
  float-division collinearity test is inexact there). Behind the grid, PIP is
  13–25% of lookups and single-digit µs either way (`geo` stays dev-oracle only).
- [x] **Real grid lookup bench** — done (`grid_bench`, ε=500 m, 2°, dominant-first):
  **0.88 µs/lookup** `-now` (6.2× vs linear) / **0.47 µs** `-1970` (4.8×); PIP needed
  24.5% / 29.7% (matches §10 P(PIP) predictions); 0 fallbacks. Found + fixed a real
  grid bug: TZBB zones deliberately **overlap** (Asia/Shanghai + Asia/Urumqi over
  Xinjiang), and a zone covering a whole cell leaves no ring for the edge walk —
  candidate sets are now edge-walk ∪ scanline-owners. After the fix, **0 wrong**
  answers; ~0.26% of lookups differ from a linear scan only inside genuine overlap
  (either tzid valid).
- [x] **Full pipeline size table** — done (`size_table`, real container, 2° grid,
  dominant-first CSR, `-now`; `-1970`/`all` skipped — `-now` suffices for stats):

  | ε(m) | quant | raw | gzip | zstd22 | br.q11 | xz9 |
  |--:|--|--:|--:|--:|--:|--:|
  | 100 | i16 | 538.2 K | 265.7 K | 251.1 K | **231.3 K** | 234.3 K |
  | 100 | i24 | 1020.0 K | 876.5 K | 872.6 K | **745.1 K** | 758.7 K |
  | 250 | i16 | 335.4 K | 199.6 K | 193.7 K | **174.7 K** | 178.2 K |
  | 250 | i24 | 605.5 K | 528.3 K | 526.1 K | **456.0 K** | 463.4 K |
  | 500 | i16 | 229.9 K | 150.3 K | 145.7 K | **133.2 K** | 134.1 K |
  | 500 | i24 | 402.1 K | 352.1 K | 349.6 K | **307.2 K** | 312.5 K |
  | 1000 | i16 | 161.8 K | 108.5 K | 106.0 K | 99.7 K | **98.5 K** |
  | 1000 | i24 | 270.5 K | 231.2 K | 229.1 K | **203.4 K** | 208.5 K |
  | 2000 | i16 | 121.7 K | 80.8 K | 78.2 K | **72.9 K** | 73.1 K |
  | 2000 | i24 | 193.1 K | 155.0 K | 152.5 K | **136.5 K** | 141.6 K |

  The ~125–460 KB goal (§1) is confirmed: ε=500 i16 brotli = 133 K, ε=100 i24
  brotli = 745 K (full-fidelity end). Note i16's quant step (~305–611 m) makes
  ε<500 m i16 quant-limited — pair i16 with ε≥500, i24 with ε≤250.
- [x] **Grid size × P(PIP) × memory** — confirmed with the real builder
  (`csr_sweep`, ε=500 m, dominant-first, 200k uniform points):

  | deg | `-now` P(PIP) | `-now` total | `-1970` P(PIP) | `-1970` total |
  |--:|--:|--:|--:|--:|
  | 1 | 13.1% | 130.1 KB | 16.5% | 140.5 KB |
  | 2 | 24.5% | 35.0 KB | 29.8% | 43.0 KB |
  | 3 | 34.4% | 17.3 KB | 40.7% | 23.4 KB |
  | 5 | 52.0% | 7.9 KB | 58.9% | 11.8 KB |
  | 10 | 83.3% | 3.2 KB | 86.6% | 5.1 KB |

  Real P(PIP) runs 2–5 pts *below* the §10 crude estimates (edge walk on raw
  geometry over-counted border cells). Totals = §10 IdSorted numbers + the
  known dominant-first cost (2°: 33.7+1.3=35.0 / 40.0+3.1=43.1 ✓). Side table
  stays ≤14 KB even at 1°; u16 tags are safe at every size (max 2,057 lists,
  5,080 ids — far under the 15-bit/u16 limits). 2° (default) and 3° both look
  right: 3° halves memory for +10 pts P(PIP).
- [x] **gzip vs zstd/brotli/xz** — answered by the same sweep: brotli q11 wins
  nearly every cell (xz9 edges it once, by 1%); zstd22 trails brotli 3–8%; gzip
  trails 5–15% but stays respectable for the smallest pure-Rust decoder
  (miniz_oxide). Balanced-preset candidate (§14.5): ε=500 m, i16, brotli → 133 K
  (gzip fallback → 150 K).
- [x] **Antimeridian** — scanned (`amscan`): TZBB with-oceans is pre-split (414/422
  verts exactly on ±180, 0 out-of-range coords). Single flagged >180° edge is
  Pacific/Auckland's south-pole seam (180,−90)→(−180,−90) — degenerate at the pole,
  planar PIP handles it. **No split pass needed.**
- [ ] (later) hierarchical grid; YStripe PIP index; `geometry-rs` comparison.

Prototypes to port from the old `formatlab` crate: `topo.rs` (topology+RDP),
quant/PIP helpers, grid/CSR (`grid2mem`/`gridsweep`), `bench`, `quant_size`,
`rdp_sweep`, `make_viewer`/sweep HTML.
