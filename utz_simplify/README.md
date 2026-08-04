# utz_simplify

Open-polyline simplification algorithms, shared between the builder
(`utz_build`, whose per-arc pass is topology-aware) and the tuning-viewer
HTML (compiled to WASM so the browser preview runs the exact code the
builder runs, with no JS reimplementation drift).

All functions take an open polyline, always keep both endpoints, and return
≥ 2 points. Units are the caller's (the builder works in degrees; convert
meters with ~111 320 m/deg and areas with its square). Three algorithms are
on the menu:

- [`rdp()`] is Ramer–Douglas–Peucker (Ramer 1972; Douglas & Peucker 1973),
  which guarantees a max perpendicular deviation ≤ ε. It is the default.
- [`visvalingam()`] is Visvalingam–Whyatt (1993), which iteratively drops
  the point spanning the smallest triangle. Its parameter is an *area*, not
  a distance: there is no ε-style deviation bound, but the result is often
  a cartographically nicer caricature at the same vertex budget.
- [`imai_iri()`] is Imai–Iri (1988), which finds the provably *minimum*
  number of vertices for a given deviation bound ε (the shortest path over
  the shortcut graph). It gives the same guarantee as RDP with
  fewer-or-equal points and more build time.

Corridor/streaming algorithms (Reumann–Witkam, Opheim, Lang, Zhao–Saalfeld)
were considered and rejected: they trade quality-per-vertex for single-pass
speed, which is worthless at build time.

Each algorithm also has a weighted variant ([`simplify_weighted()`],
`*_w`): a per-vertex tolerance multiplier `weights[i]` makes the effective
parameter `eps * weights[i]` (for Visvalingam it is
`min_area * weights[i]²`, because areas scale as distance²). The builder
uses this for population-density-aware refinement: denser areas get smaller
multipliers, so boundaries stay precise where people live.
`weights[i] = 1.0` everywhere reproduces the scalar functions exactly.

[`rdp()`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.rdp.html
[`visvalingam()`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.visvalingam.html
[`imai_iri()`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.imai_iri.html
[`simplify_weighted()`]: https://docwilco.github.io/utz/docs/utz_simplify/fn.simplify_weighted.html

License: MIT
