# Spatial upgrade contract

Status: released in RomeDB 2.5.0 for Linux x64 ([release record](release-2.5.0.md)).
Implementation status is tracked in [the task checklist](spatial-upgrade-tasks.md). The planning record is
`plan-spatial-upgrade.md` in the parent planning directory.

This contract revises the 2.2.0 decision that general geometry is out of scope
([decision record](v2.2-decisions.md#additional-temporaldecimal-value-domains-and-general-geometry-wont-do)).
The user authorized the revision on 2026-10-12. It covers only the bounded subset
below: Point, Polygon and MultiPolygon values, point-in-region predicates,
viewport search, exact nearest search, and additional H3 cell functions. General
GIS remains out of scope. The released point contract in [v2-spatial.md](v2-spatial.md)
is unchanged.

## 1. Geometry model

- Coordinates are two-dimensional WGS84 longitude/latitude degrees, longitude
  first. Longitude is in [-180, 180] and latitude in [-90, 90]. Positions hold
  exactly two finite numbers. Integers are accepted and stored as float64.
  Altitude, numeric strings, nonfinite values and out-of-range values reject.
  Stored coordinates are never wrapped or rounded.
- Geometry values are ordinary objects with exactly `type` and `coordinates`
  members, the same encoding as the released Point. There is no new storage tag,
  wire tag or client value type.
- Supported types are Point, Polygon and MultiPolygon. LineString,
  MultiLineString, MultiPoint and GeometryCollection reject.
- Polygon edges are straight lines in longitude/latitude space. Distance uses
  the released sphere of radius 6,371,008.8 m. Neither is ellipsoidal.

## 2. Polygon validity

A ring is a closed sequence of positions. A Polygon has one exterior ring and
zero or more holes. Validation rejects, and never repairs:

- Rings with fewer than four positions, or whose last position differs from the first.
- Consecutive repeated positions (zero-length edges).
- Rings without three non-collinear positions.
- Any contact between non-adjacent edges of one ring, and backtracking
  (collinear overlap) between adjacent edges.
- Any contact between two rings of one Polygon, including a single shared point.
- Holes outside their exterior ring, and holes inside other holes.
- MultiPolygon components whose edges cross or share a segment, or whose
  interiors overlap. Components may touch at isolated points. A component may
  lie inside another component's hole.
- Edges spanning 180 or more degrees of longitude. Split antimeridian regions
  into MultiPolygon components at longitude 180 and -180.
- Any position at latitude 90 or -90. Polar regions are not supported.

Ring winding is accepted in either direction, and predicates do not depend on
it. Output preserves the input position order exactly; it does not reorient.
Errors name the ring, and the polygon for MultiPolygon, where practical.

Orientation and segment tests are exact. A floating-point filter decides
ordinary cases; inputs it cannot classify are evaluated with exact integer
arithmetic over the binary64 values. No tolerance or epsilon is used.

## 3. Predicate semantics

| Point position | contains / inside | covers / intersects |
| --- | --- | --- |
| Interior, outside every hole | true | true |
| On an exterior edge or vertex | false | true |
| Inside a hole | false | false |
| On a hole edge or vertex | false | true |
| Outside every component | false | false |

Longitudes -180 and 180 are the same place for a tested point. A point on a
split seam is on the boundary of the component it touches. The predicates do not
reconstruct the interior of the original unsplit region.

Predicates compare one Point with one Polygon or MultiPolygon. Point/Point and
region/region pairs are errors, not false results.

## 4. Functions

Native names are canonical. Each `ST_*` name is an alias that runs the same
operation, so planner recognition later applies to both spellings. `ST_*` names
are matched without regard to case.

| Native | PostGIS alias | Arguments | Result |
| --- | --- | --- | --- |
| `geo::point` (released) | `ST_Point`, `ST_MakePoint` | lng, lat. `ST_Point` also accepts SRID 4326 | Point |
| `geo::lng`, `geo::lat` | `ST_X`, `ST_Y` | Point | number |
| `geo::from_geojson` | `ST_GeomFromGeoJSON` | GeoJSON geometry text | geometry |
| `geo::to_geojson` | `ST_AsGeoJSON` | geometry | text |
| `geo::envelope` | `ST_MakeEnvelope` | west, south, east, north. Alias also accepts SRID 4326 | Polygon |
| `geo::distance` (released) | `ST_DistanceSphere` | Point, Point | meters |
| `geo::within` (released) | `ST_DWithin` | Point, Point, meters. Alias also accepts `use_spheroid` false | boolean |
| `geo::contains` | `ST_Contains` | region, Point | boolean |
| `geo::covers` | `ST_Covers` | region, Point | boolean |
| `geo::inside` | `ST_Within` | Point, region | boolean |
| `geo::intersects` | `ST_Intersects` | Point and region, either order | boolean |
| `geo::cell`, `geo::cell_center` (released) | | | unchanged |
| `geo::cell_boundary` | | cell | Polygon, or MultiPolygon split at 180 |
| `geo::cell_parent` | | cell, resolution | cell |
| `geo::cell_children` | | cell, resolution | array of cells |
| `geo::cell_disk` | | cell, k | array of cells within k grid steps |
| `geo::cell_resolution` | | cell | integer |
| `geo::cell_distance` | | cell, cell | integer grid steps |
| `geo::cell_area` | | cell | square meters |
| `geo::cell_compact` | | array of cells at one resolution | array of cells |
| `geo::cell_uncompact` | | array of cells, resolution | array of cells |

`geo::within` keeps its released radius meaning. The point-in-region inverse of
contains is `geo::inside`, because the three-argument radius form already exists.

### Differences from PostGIS

- `ST_DistanceSphere` uses radius 6,371,008.8 m. PostGIS derives its radius from
  the SRID spheroid, which is within 0.01 m of this value for SRID 4326.
- `ST_DWithin` measures on the sphere. PostGIS geography `ST_DWithin` defaults to
  the spheroid; passing `use_spheroid => true` here is an error.
- `ST_AsGeoJSON` does not round. PostGIS defaults to nine decimal digits. Extra
  arguments are not accepted.
- `ST_Point` accepts only SRID 4326. `ST_MakePoint` accepts two arguments only.
- There is no bare `ST_Distance`; its PostGIS geometry meaning is planar degrees.
- Text is not implicitly converted to geometry. Use `ST_GeomFromGeoJSON`.
- WKT, WKB, `ST_Transform`, other SRIDs, buffers, unions, intersections, areas,
  centroids and line geometries are not provided.

### GeoJSON rules

- Input is one geometry object. Feature, FeatureCollection and
  GeometryCollection reject; extract a Feature's `geometry` first.
- Malformed JSON and duplicate object keys reject at any depth.
- A `crs` member rejects. `bbox` and foreign members are ignored and discarded.
- Output is `{"type":...,"coordinates":...}` with shortest round-trip number
  formatting. Parsing the output reproduces the same coordinate values.

### NULL and errors

- The five released functions keep their released behavior: NULL arguments are
  errors and errors keep their released codes.
- Every other function, including every `ST_*` alias of a released function,
  returns NULL when any argument is NULL.
- Non-NULL invalid input reports `FDB_VALIDATION`. A resource limit reports
  `FDB_LIMIT`.

## 5. Limits

Initial limits, frozen for S0 from the cost assessment in section 10. They can
change only with new measurements recorded here.

| Resource | Limit |
| --- | --- |
| GeoJSON input | 1 MiB of text, nesting depth 32 |
| Positions per geometry | 100,000 |
| Rings per geometry | 10,000 |
| MultiPolygon components | 1,000 |
| Topology validation work | 20,000,000 segment and point checks |
| Cell arrays, input and output | 10,000 cells |
| Prepared regions | 4,096 per thread, 32 MiB of encoded regions, least recently used first out |

Validation counts its work; a vertex limit alone does not bound it. Exceeding
any limit fails the statement with `FDB_LIMIT` and never returns partial output.

A region is validated once and reused while the same encoded value repeats
within a thread: a constant query region across the rows of one statement, and
stored zones across statements. Reuse is keyed by the complete encoded bytes, so
different values never share a prepared region.

## 6. Indexed sources

| Source | Result | Contract |
| --- | --- | --- |
| `search::near(index, center, radius_m)` (released) | `id`, `distance_m` | Unchanged results. Uses the two-dimensional index when the index has the new format |
| `search::bbox(index, west, south, east, north)` | `id` | Complete points in the inclusive viewport. `west > east` crosses the dateline. `west = -180, east = 180` covers all longitudes. Equal bounds mean zero width or height. Points at longitude -180 and 180 match both seams |
| `search::polygon(index, region, mode)` | `id` | Complete points with mode `'contains'` or `'covers'`. The region is a Polygon or MultiPolygon value or constructor |
| `search::nearest(index, center, k[, allowed_ids])` | `id`, `distance_m` | Complete `k` nearest points ordered by distance then typed ID. `k` is an integer from 1 to 10,000. The optional typed-ID array works as in `search::text` and `search::vector` |
| `search::containing(index, point, mode)` | `id` | Complete stored regions of a region index that contain the point (mode `'contains'`) or cover it including the boundary (mode `'covers'`) |

Arguments are literals, bound parameters, numeric signs, parentheses and the
point, envelope and GeoJSON constructors in either spelling. Sources require a
managed spatial index and never fall back to a scan. Released latitude-format
indexes serve all four sources with their latitude range; only the new format
prunes longitude.

Like `search::text` and `search::vector`, an explicit source sees only its own
arguments. Outer `WHERE` conditions filter the rows it returns, so a filtered
nearest source can return fewer than `k` rows. Filter-aware nearest search is the
ordinary SQL form below.

`search::nearest` runs complete radius searches from 100 m until a round
returns `k` rows or the radius covers the globe. The radius quadruples after an
empty round; otherwise it grows by the square root of the shortfall times 1.25,
between two and eight times. Rounds and the outer statement run in one savepoint, so
they read one snapshot that includes the caller's pending writes.

## 7. Ordinary SQL index use

The planner recognizes these forms on a top-level indexed field, written
directly or qualified by the table name or alias, in a single-table SELECT. On a
point index:

- `ST_Covers(region, field)`, `ST_Contains(region, field)`,
  `ST_Within(field, region)` and `ST_Intersects` in either argument order, and
  their `geo::*` spellings.
- `ST_DistanceSphere(field, point) <= radius`, also with `<`, reversed
  comparisons and either argument order.
- `ST_DWithin(field, point, radius)` in either argument order, with an optional
  `false` fourth argument.
- `ORDER BY distance(field, point) [, id] LIMIT k` in ascending order, where the
  distance is written directly or through a projected alias, `k` is a literal or
  parameter from 1 to 10,000, and there is no OFFSET, GROUP BY, DISTINCT, window or
  aggregate. The WHERE clause must exclude missing locations, either with
  `ST_DistanceSphere(field, point) IS NOT NULL` or with one of the conditions
  above. Either distance spelling may appear in ORDER BY.

On a region index (section 8), with a constant point:

- `ST_Covers(field, point)`, `ST_Contains(field, point)`,
  `ST_Within(point, field)` and `ST_Intersects` in either argument order, and
  their `geo::*` spellings.

A recognized condition must be one of the top-level AND terms, and its query
region, point and radius must be constant for the execution: literals,
parameters or constructors. The rewrite restricts the collection to the IDs the
index search returns, as a semijoin on the stored ID column, so results and
errors are unchanged:

- Conditions that the index search answers exactly (every form above except a
  strict `<` or `>` distance comparison) are replaced by the semijoin, because
  the search refines with the same stored coordinates or shapes. Other
  conditions stay and are evaluated on the candidates.

- The spatial index validates every stored value on write, so no skipped
  document could have raised an error.
- A NULL, invalid or non-constant query argument leaves the statement to
  ordinary evaluation. NULL arguments therefore return no rows through a scan.
- `geo::distance(...) <= r` and `geo::within(...)` are not rewritten in WHERE,
  because they raise errors on missing locations that a rewrite would skip. Use
  `ST_DistanceSphere`, `ST_DWithin` or `search::near`.
- `NOT INDEXED` and `INDEXED BY` on the table disable the rewrite.
- Nearest statements first find the radius that holds `k` indexed points with
  raw index rounds, because filters only remove points. They then probe complete
  radius rounds with the statement's own filters from that radius, and run with
  the first radius that yields `k` rows. Rounds grow by the observed shortfall,
  at least doubling. Everything runs in one savepoint. EXPLAIN shows the first
  round at its nominal 100 m radius.
- EXPLAIN reads no data (2.5.2). It shows the spatial plan, including the first
  nearest round of `search::nearest` and of ordinary nearest statements at the
  nominal radius. Choices that count data are made only when the statement runs:
  the starting and later radii, the scalar index preference and the early scan.
  An EXPLAIN can therefore name the spatial index for a statement that runs
  through a scalar index or a scan. 2.5.0 and 2.5.1 read spatial index entries,
  and 2.5.1 also counted scalar matches, during EXPLAIN.
- Filters that match few documents (2.5.1). After a round that finds fewer than
  `k` rows, the planner counts that round's candidates. Reading a candidate by ID
  costs about eight sequential scan steps, so once the candidates read so far, or
  the number the observed match rate predicts for the next round, reach an eighth
  of the index entries, the statement uses the ordinary plan instead. That plan
  evaluates the distance guard after the other conditions.
- Scalar indexes (2.5.1). When another top-level condition is an equality or IN
  with constants on a top-level field that has a scalar index, the planner counts
  the documents that condition selects, up to 4,096. A region or radius condition
  keeps its spatial search only if the search returns no more candidates than the
  scalar index selects. A nearest statement uses the scalar index plan when it
  selects fewer than 4,096 documents and the square of that count is below
  `2 * k` times the spatial index entries; the rounds would read about
  `k * entries / count` candidates. Otherwise the spatial candidates drive the
  statement, and the scalar index is not scanned.

RomeDB rejects `field IS NOT NULL` when the field holds an object, so the NULL
exclusion is written on the distance value. Statements outside these forms keep
ordinary SQL semantics, and EXPLAIN shows any scan.

## 8. Index format and migration

- New spatial indexes use the two-dimensional format: Morton keys over 31-bit
  quantized longitude and latitude, with the original float64 longitude and
  latitude in each entry. The native index covers the key, both coordinates and
  the record ID, so range scans refine candidates without reading table rows.
  Missing and NULL fields keep a NULL entry for auditing, as in the released format.
- The index records `spatial: {"format": 2}` in its catalog entry. Its collection
  moves to catalog version 7. RomeDB 2.4.0 and older accept versions 1 to 6, so
  they reject such a collection instead of misreading it. Keep a backup before
  creating or rebuilding a spatial index if older binaries must open the file.
- Released latitude-format indexes keep working with unchanged results and keep
  catalog version 3. Their entries are never reinterpreted.
- `REINDEX name` rebuilds a spatial index in the new format inside one
  transaction, validating every point before publishing the new catalog entry,
  in the same way it upgrades full-text storage.
- `CREATE SEARCH INDEX IF NOT EXISTS` never rebuilds or upgrades an existing index,
  and fails when the existing index has the other geometry kind.

### Region indexes

`CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')`
indexes Polygon and MultiPolygon values for finding the stored regions that
contain a point (reverse geofencing). Missing and NULL values are excluded; Points
and invalid geometries are rejected on write and at index creation.

- Each region is covered by at most 32 quadtree cells over the same quantized
  grid, stored as level-tagged cell codes with the record ID in a covering
  B-tree. A side table keeps each document's canonical shape for refinement.
- A lookup reads the 32 cells, one per level, that contain the point (64 for a
  point on the antimeridian, which matches both seams), then tests each candidate
  shape exactly. Cells may admit extra candidates; they never omit a region.
- The catalog entry is `spatial: {"format": 2, "regions": true}`, at catalog
  version 7. `REINDEX` rebuilds it in the same format.
- Point sources reject region indexes and `search::containing` rejects point
  indexes.
- `INFO FOR TABLE` and `INFO FOR INDEX` report `geometry` (`point` or `region`)
  and `format` (`latitude` for the released 2.4.0 format, `two_dimensional`
  otherwise) for every spatial index (2.5.1).

### Index maintenance

Updates and deletes maintain scalar and point-index entries by their key
columns, so they seek through the entry index instead of scanning the entry
table for the record ID. An update that leaves an index key unchanged leaves its
entry untouched. If a stored entry does not match its document's keys, the
entry is deleted by record ID instead, so maintenance never depends on index
content being correct. This applies to every collection and needs no format
change.
- Integrity audits and reopen checks compare every entry, including both
  coordinates and the key, with its document, and validate the storage schema
  of either format.

## 9. Profiles

`profile_select` reports two spatial fields, also exposed by the Node client as
`spatialRanges` and `spatialRounds`:

- `spatial_ranges`: index key ranges searched by spatial sources and rewrites,
  summed over every round. A latitude-format search counts one range.
- `spatial_rounds`: radius rounds run by `search::nearest` and by ordinary
  nearest statements before the final statement.

The engine's own fields describe the final statement:

- `index_steps` counts index entries read. For a spatial search these are its
  candidates. Each candidate is refined exactly once against the stored
  coordinates or shape, so refinement needs no separate counter.
- `rows_read` counts table rows and index entries read, which includes the
  documents fetched for the remaining conditions and the result.
- Planning probes, the rounds before the final statement and the bounded counts
  for scalar indexes are not included in `rows_read` or `index_steps`. They add
  to `spatial_rounds` and `spatial_ranges`.
- Metered reads charge all of that work, including `search::nearest` rounds, to
  the statement's read and VM budgets (2.5.1; 2.5.0 charged only the final
  statement). EXPLAIN performs none of this work (2.5.2), so metered EXPLAIN
  reports zero rows read.

## 9a. Deferred functions

`geo::cell_cover(region, resolution)` is not added. It would return the H3 cells
whose centers lie in a region, which is an aggregation convenience rather than an
index cover, because center containment can omit cells that the region crosses.
The same aggregation is available by grouping an indexed region search by
`geo::cell(location, resolution)`, and no application in the planned examples
needs the cell list itself. It can be reconsidered with a budgeted fill from
`geo::cell_disk` if users ask for it. Merging cells into an outline needs polygon
union, which stays excluded.

## 10. Initial cost assessment

Measured on 2026-10-12 with the `romedb-production` profile (optimized, with
debug assertions and overflow checks), single-threaded, on an Intel Xeon E5-2673 v4
under the shared four-CPU build limit. Command:

```sh
cargo test --locked --profile romedb-production -p romedb --lib geometry_cost -- --ignored --nocapture
```

Point location used 100,000 grid points across each shape.

| Shape | Validate and prepare | Locate one point |
| --- | --- | --- |
| Outline with short edges, 1,000 vertices | 0.8 ms | 136 ns |
| Outline with short edges, 10,000 vertices | 4.6 ms | 75 ns |
| Outline with short edges, 99,998 vertices | 45 ms | 174 ns |
| Star whose edges each span 40% of its height, 1,000 vertices | 2.2 ms | 1.8 µs |
| Same star, 10,000 vertices | 181 ms | 14 µs |
| Same star, 99,998 vertices | `FDB_LIMIT` after 94 ms | |
| MultiPolygon of 1,000 squares | 4.6 ms | |

Ordinary outlines are cheap at the position limit. Long overlapping edges defeat
both the validation sweep and the latitude strips, so cost grows faster than the
vertex count. The work limit stops that case in under 0.1 s instead of letting it
run quadratically. These are development measurements, not latency promises.

## 11. Example coverage

| Application example | Planned test |
| --- | --- |
| Female profiles within 5 km | Released `search::near` join, and the ordinary `ST_DistanceSphere <= r` rewrite |
| Five nearest available, recently seen drivers | Ordinary nearest form with filters; explicit `search::nearest` without filters |
| Map viewport | `search::bbox`, including dateline, seam and full-world viewports |
| Houses covered by a storm polygon | Indexed `ST_Covers` rewrite and `search::polygon` |
| Which stored zone contains a point | Region index with `search::containing` and the indexed `ST_Covers(shape, point)` rewrite |
| H3 dashboard counts | Released `geo::cell` grouping, optionally over `search::bbox` rows |
