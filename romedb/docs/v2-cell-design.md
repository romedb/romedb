# V2 cell aggregation design review

Status: design selected for V2-S3; compatibility probe passed, integration is pending.
This does not change the already implemented latitude B-tree radius index.

| Candidate | Fit and tradeoff |
|---|---|
| H3 | Standard global grid with resolutions 0–15 and interoperable cell addresses. Suits spatial aggregation; hexagonal hierarchy has approximate geometric containment between resolutions, so it must not replace exact radius refinement. |
| S2 | Global square-cell hierarchy. Viable alternative, but no selected RomeDB workload currently requires its hierarchy instead of H3. |
| Geohash/rectangular degree bins | Simple representation, but latitude-dependent cell shape and pole/dateline handling would become an additional RomeDB-specific contract. |
| Latitude B-tree | Already implemented for conservative radius candidates; it does not define global aggregation cells. |

Choose H3 cell addresses for aggregation, using the Rust `h3o` implementation if a
pinned release supports Rust 1.88.0 and passes reference fixtures. Check dependency
licenses and include notices before releasing. No unmeasured performance advantage
is claimed. H3's logical parent hierarchy is not exact geometric containment.

Adopted API, implemented in the V2 working tree:

- `geo::cell(point, resolution)` returns a lowercase H3 cell-address string;
  resolution is an integer from 0 through 15.
- `geo::cell_center(cell)` returns the existing point object in longitude/latitude
  order. Validate cell mode/address rather than accepting arbitrary hexadecimal.
- Use normal `GROUP BY geo::cell(location, $resolution)` with COUNT/SUM for cell
  aggregation. No separate result shape or implicit resolution change.
- Match upstream reference cell IDs; test equivalent +/-180 longitudes and poles,
  centers returning to their own cell, invalid resolutions/addresses, persistence,
  grouped queries and both Node clients. Fixed-resolution boundary assignment
  follows H3; do not infer a radius covering from nearby cell centers.

References reviewed 2026-09-25:
[H3 introduction and alternatives](https://h3geo.org/docs/),
[H3 hierarchy and geometric containment](https://h3geo.org/docs/highlights/indexing/),
[H3 indexing APIs](https://h3geo.org/docs/api/indexing/),
[h3o LatLng API](https://docs.rs/h3o/latest/h3o/struct.LatLng.html).

## Dependency compatibility evidence

A temporary project at `/tmp/romedb-v2-h3-probe` tested the real packages with
Rust 1.88.0, without modifying RomeDB's manifest or lockfile:

- `h3o 0.10.0` with default features first resolved `ordered-float 5.5.0`, which
  requires Rust 1.90. Pinning that dependency to 5.0.0 exposed unsupported const
  floating-point calls in h3o itself.
- `h3o 0.9.4` with default features similarly requires a newer const `round`.
- **`h3o = { version = "=0.9.4", default-features = false }` passed**, using its
  libm math implementation. The official latitude 45/longitude 40/resolution 2
  example produced `822d57fffffffff`, and its center mapped back to that cell.
  Log: `/tmp/romedb-v2-h3-probe-libm.log`.

This exact configuration is now in the frontend manifest and lockfile. The lock
adds only h3o, h3o-bit and float_eq; existing libm/either dependencies are reused.
The Linux Node inventory and checksummed notice bundle include the three added
packages (BSD-3-Clause for h3o/h3o-bit, MIT OR Apache-2.0 for float_eq).
The probe is preliminary evidence; integrated qualification is recorded in
[the cell/projection milestone evidence](v2-cell-projection-evidence.md).
