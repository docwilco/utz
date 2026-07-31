# utz-simplify

Open-polyline simplification algorithms, shared between the
builder (`utz-build`, per-arc topology-aware pass) and the tuning-viewer
HTML (compiled to WASM so the browser preview runs the exact code the
builder runs: no JS reimplementation drift).

All functions take an open polyline, always keep both endpoints, and return
≥ 2 points. Units are the caller's (the builder works in degrees; convert
meters with ~111 320 m/deg, areas with its square). The menu:

- [`rdp`]: Ramer–Douglas–Peucker (Ramer 1972; Douglas & Peucker 1973):
  max perpendicular deviation ≤ ε guaranteed. The default.
- [`visvalingam`]: Visvalingam–Whyatt (1993): iteratively drop the point
  spanning the smallest triangle. Parameter is an *area*, not a distance:
  no ε-style deviation bound, but often a cartographically nicer caricature
  at the same vertex budget.
- [`imai_iri`]: Imai–Iri (1988): the provably *minimum* number of vertices
  for a given deviation bound ε (shortest path over the shortcut graph).
  Same guarantee as RDP, fewer-or-equal points, more build time.

Corridor/streaming algorithms (Reumann–Witkam, Opheim, Lang, Zhao–Saalfeld)
were considered and rejected: they trade quality-per-vertex for single-pass
speed, which is worthless at build time.

Each algorithm also has a weighted variant ([`simplify_weighted`], `*_w`):
a per-vertex tolerance multiplier `w[i]` makes the effective parameter
`eps * w[i]` (Visvalingam: `min_area * w[i]²`, areas scale as distance²).
The builder uses this for population-density-aware refinement: denser
areas get smaller multipliers, so boundaries stay precise where people
live. `w = 1.0` everywhere reproduces the scalar functions exactly.

[`rdp`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.rdp.html
[`visvalingam`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.visvalingam.html
[`imai_iri`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.imai_iri.html
[`simplify_weighted`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.simplify_weighted.html

License: MIT
