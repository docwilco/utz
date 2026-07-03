# uTZ — micro-timezone

A tiny, embeddable latitude/longitude → IANA timezone lookup crate. Ground-up
rewrite; keeps only the *contract* of the old `spatialtime` (Reader-style API,
OSM source), replaces the whole engine.

Working crate name: **`utz`** (project: uTZ / micro-timezone).

---

## 1. Goals

- **Tiny.** OSM timezone data down to ~125–460 KB (vs tzf-rs ~5–7 MB) via
  shared-arc topology + tunable RDP + integer quantization + general compression.
- **Embeddable / `no_std`-friendly.** Pure-Rust codecs, integer PIP, flat arrays
  that borrow zero-copy from a flash partition. Targets ESP32/Xtensa-class.
- **Tunable at build time.** Dataset, RDP ε, quant grid, grid cell size, codec —
  build exactly the size/RAM/accuracy point you need, guided by a viz tool.
- **DST-correct.** Returns the IANA `tzid`; resolve offsets/DST downstream
  (`chrono-tz`). `-1970` option gives per-location-correct tzids.

Non-goals: shipping committed assets; NED dataset (dropped — RDP+topology on OSM
is ≤ NED size at real fidelity); being the batch-throughput champion (tzf/rtz
exist for server use).

---

## 2. Workspace layout (flat, no `crates/` subdir)

```
utz/                 workspace root
  Cargo.toml         [workspace] members = ["utz", "utz-build"]
  PLAN.md            this file
  tz.toml            (optional) asset-knob overrides read at build time
  .gitignore         cache/, assets/, *.geojson, *.utz, viewers

  utz/               runtime library crate
    build.rs         download (cond-GET) + cache + invoke utz-build + embed
    src/
      lib.rs         public API, feature gates, grouped compile_errors
      decompress.rs  codec backends (uncompressed/gzip/zstd/brotli/xz)
      format.rs      self-describing header, zone table, arc store, ring index
      grid.rs        ndarray cell → zone-id | spill-index (+ CSR spillover)
      pip.rs         hand-rolled per-polygon integer PIP (i64/i128)
      finder.rs      Finder: new()/from_static()/from_reader() + lookup()/fuzzy

  utz-build/         build-dependency + dev/exploration + viz tool
    src/
      lib.rs         re-exports encoder + measurement helpers
      types.rs       Feat/Ring/Poly, quantization helpers
      loader.rs      source → Vec<Feat>  (geojson; fgb reader kept for now)
      topo.rs        shared-arc topology + topology-aware RDP (ported)
      rdp.rs         open-polyline RDP
      grid.rs        grid + interned-CSR builder
      encode.rs      container serializer (header + sections + compress)
      download.rs    conditional GET (ETag / Last-Modified)
      viz.rs         emit the tuning HTML
    examples/        measurement binaries (bench, sweeps) — continue here
```

`utz-build` is where the exploration/measurement continues (the `formatlab`
prototypes get ported here). It is both a build-dependency of `utz` and the home
of the viz tool and benchmarks.

---

## 3. Public API

Self-describing format ⇒ one `Finder` type, any variant, multiple sources:

```rust
impl Finder {
    fn new() -> Result<Finder>;                    // embedded asset (feature `embed`)
    fn from_static(bytes: &'static [u8]) -> Result<Finder>;  // flash partition: ZERO-COPY (uncompressed)
    fn from_reader(r: impl Read) -> Result<Finder>;          // file / network / OTA: owned buffer

    fn lookup(&self, lon: f64, lat: f64) -> Option<&str>;    // accurate: grid → PIP
    fn fuzzy(&self,  lon: f64, lat: f64) -> Option<&str>;    // grid-only, no geometry, ~cell-size error
}
```

- **`from_static`** is the embedded/flash win — borrows the bytes, no RAM copy in
  `uncompressed` mode. `impl Read` can't zero-copy, so it's the std/OTA path.
- **`no-embed` mode:** ship the binary *without* the asset, load it at runtime from
  a flash partition (`from_static`) or file (`from_reader`). Enables **OTA-updatable
  tz data** (swap `-now`↔`-1970`, new TZBB vintage) without reflashing firmware.
- **`fuzzy`** (learned from tzf's FuzzyFinder): answer from the grid alone — no arcs
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
header:      magic, version, dataset(now|1970), tzbb_release,
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

## 5. Build pipeline (build time)

1. **Download** `timezones-with-oceans-{now,1970}.geojson.zip` → `cache/`.
   **Conditional GET**: store `ETag`/`Last-Modified`, send
   `If-None-Match`/`If-Modified-Since` → 304 reuses cache. Record TZBB release in
   the header (DST vintage + cache-invalidation).
2. **Parse** → features (tzid + MultiPolygon).
3. **Topology**: dedup vertices, cut shared arcs at junctions.
4. **Topology-aware RDP** at ε (each arc simplified once → borders stay stitched).
5. **Quantize** arcs (i16/i24/i32), delta + zigzag-varint.
6. **Grid** at cell size: rasterize borders → `Array2<u16>` (zone-id | spill-index)
   + interned-CSR spillover.
7. **Serialize** self-describing container → **compress** (chosen codec) → `OUT_DIR`
   → `include_bytes!` (when `embed`).
8. **Two-level cache**: cache the built artifact keyed by hash(TZBB release + all
   knobs), so unchanged rebuilds are instant.
- **docs.rs**: skip download/build, embed empty slice.
- **Cost note**: first build is heavy (47 MB zip, 156 MB json, topology over
  millions of verts + q11 compression). The two caches make it one-time.

---

## 6. Datasets

- **`-now`** (65 zones): merges currently-clock-identical zones → smaller,
  representative tzids (e.g. Amsterdam→`Europe/Paris`).
- **`-1970`** (304 zones): merges only zones identical since 1970 → matches IANA's
  own equivalence, **correct per-location tzid** (`Europe/Amsterdam`,
  `America/Detroit`). Bigger but faithful.

Exactly one selected. `-now` default (smallest); document the tzid-representative
caveat so users who need exact names pick `-1970`.

**Source URLs** (timezone-boundary-builder, `releases/latest/download/`, GeoJSON zip).
Six variants: {land-only, with-oceans} × {full, -1970, -now}. uTZ uses the
**with-oceans** ones (global coverage — land-only leaves the sea uncovered):

```
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-1970.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-now.geojson.zip
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans.geojson.zip        # (full, unmerged ~450 zones)
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans-1970.geojson.zip   # uTZ `1970`
https://github.com/evansiroky/timezone-boundary-builder/releases/latest/download/timezones-with-oceans-now.geojson.zip    # uTZ `now` (default)
```

(`full` with-oceans = one polygon per IANA zone, ~450 — not exposed by default, but
the pipeline could offer it as a third dataset for maximum tzid fidelity.)

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

Decision: **both eager and lazy, feature-selected**, plus `fuzzy`.
- **Lazy** (`lazy`): grid → candidate ids → decode **one polygon at a time** from
  the arc store (resolve arc refs → i64), PIP, discard. Working set = largest single
  candidate polygon. Interior cells decode **zero** geometry. Best for embedded.
- **Eager** (`eager`): decode all polygons into RAM in `new()`. Fastest repeat
  lookups, highest RAM. Server/std.
- **Fuzzy**: grid-only, no arcs. ~cell-size error, ~KBs, no geometry loaded.
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
  until a finer grid / full-unmerged OSM makes the pool the bottleneck.

---

## 11. Features & config (leaning features; a couple in `tz.toml`)

No default features. Grouped `compile_error!`s that **list the options**:
```rust
compile_error!("select a dataset: enable exactly one of `now` or `1970`");
compile_error!("select a codec: one of `uncompressed`,`gzip`,`zstd-sys`,`ruzstd`,`brotli`,`xz`");
```

- **Cargo features (discrete):** dataset (`now`/`1970`), codec (+ size tiers),
  quant (`i16`/`i24`/`i32`), grid (`g1`/`g2`/`g3`/`g5`/`g10`), memory mode
  (`eager`/`lazy`/`fuzzy`), `embed`/`no-embed`. RDP presets (`rdp-100`/`rdp-250`/…).
- **`tz.toml` (continuous / rare overrides):** an arbitrary `rdp_meters` not covered
  by a preset, custom grid degree, TZBB URL override. `build.rs` reads it,
  `rerun-if-changed`.

**Trade-off (discussion continues):**
- *Features* → reproducible (in Cargo.toml / lockfile), discoverable (docs.rs), but
  discrete and combinatorial; each "one-of-N" axis needs mutual-exclusion boilerplate.
- *Env vars* → flexible/continuous, but ambient & non-reproducible (not captured in
  Cargo.toml; CI must set them).
- *Config file* → flexible **and** reproducible (committed), but another mechanism.
- Because the format is **self-describing**, knobs never change decode code — so
  whichever mechanism sets them, the runtime stays generic. Leaning: **features for
  the discrete knobs + `tz.toml` for the continuous overrides.** Not final.

---

## 12. Visualization

`utz-build/viz.rs` + `cargo run -p utz-build --example visualize` regenerates the
sweep viewer (RDP levels × quant grids × basemap, our current HTML: keyless
Carto/Esri tiles, scale bar, on-the-fly JS quantization, full stays f64). Users
tune ε/quant/grid **before** committing the build knobs. Link a CI-built copy from
docs (HTML self-embeds data → generated artifact, not a committed asset).

---

## 13. How uTZ differs from tzf-rs (why build it)

Win: **~10× smaller** (general compression tzf lacks + tunable aggressive RDP + int
quant): ~125–460 KB vs tzf ~5–7 MB. **Genuinely `no_std`/flash-embeddable** (tzf is
std/protobuf, can't zero-copy from flash, can't run on ESP32). **Tunable** to an
exact size/RAM/accuracy point. **`-1970`** for correct per-location tzid.
Not-better: tzf is mature/tested; we reuse its good ideas (topology, 1° grid,
delta-varint); if you don't need embedded/tiny, tzf already exists.

**Learned from tzf (adopt now):** grid-only `fuzzy` mode; ship a balanced preset;
embed TZBB version; verify antimeridian handling. **Defer:** hierarchical/quadtree
grid (1°-accuracy at coarse memory); per-polygon YStripe edge index (faster PIP on
big polygons); benchmark `geo` vs `geometry-rs`.

---

## 14. Open decisions (continue later)

1. **Build-knob mechanism** — features vs `tz.toml` split (leaning features + toml).
2. **`geo` vs hand-rolled PIP** at runtime — leaning hand-rolled i64 (no_std), geo as
   dev-oracle. Confirm with a speed bench.
3. **`LonLat` newtype** vs raw `(lon, lat)` to prevent order footgun.
4. **Antimeridian**: verify TZBB is pre-split; else add a split pass.
5. **Default preset** values (dataset/ε/quant/grid/codec) for the "balanced" build.
6. Crate/repo name confirmed `utz`; public naming of feature groups.

---

## 15. Measurement backlog (do in this workspace)

- [ ] **Dominant-first interning cost** — extra unique lists + KB at 2° (`-now`/`-1970`).
- [ ] **Hand-rolled i64 PIP vs `geo`** — correctness (0 disagreements target) + speed.
- [ ] **Real grid lookup bench** — interior O(1) vs border PIP, vs linear scan, both datasets.
- [ ] **Full pipeline size table** — topology×RDP(ε)×quant(i16/i24)×codec(incl gzip), `-now`/`-1970`.
- [ ] **Grid size × P(PIP) × memory** confirmation with the *real* CSR builder.
- [ ] **gzip** vs zstd/brotli/xz on the container.
- [ ] **Antimeridian** scan of TZBB polygons.
- [ ] (later) hierarchical grid; YStripe PIP index; `geometry-rs` comparison.

Prototypes to port from the old `formatlab` crate: `topo.rs` (topology+RDP),
quant/PIP helpers, grid/CSR (`grid2mem`/`gridsweep`), `bench`, `quant_size`,
`rdp_sweep`, `make_viewer`/sweep HTML.
