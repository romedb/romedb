# V2 spatial contract

Released in RomeDB 2.0.0 and unchanged in behavior through 2.5.0: point scalar
functions, managed radius search and H3 cell aggregation.

RomeDB 2.5.0 adds Polygon and MultiPolygon values, point-in-region predicates,
`ST_*` aliases, more H3 functions, a two-dimensional index format and region
indexes without changing the results of anything below. Spatial indexes created
or rebuilt by 2.5.0 use catalog version 7. See
[the spatial upgrade contract](spatial-upgrade.md).

- `geo::point(longitude, latitude)` returns an ordinary object with `type: 'Point'`
  and `coordinates: [longitude, latitude]`, both stored as float64 numbers.
  Coordinates follow [GeoJSON longitude/latitude order](https://www.rfc-editor.org/rfc/rfc7946#section-3.1.1)
  in WGS84 degrees. Only two-dimensional points are supported.
- Arguments must be finite numbers (integers accepted); longitude must be within
  [-180, 180], latitude within [-90, 90]. No string coercion or coordinate wrapping.
- `geo::distance(point, point)` returns meters along a sphere of radius
  6,371,008.8 m, using the haversine central angle. This is an explicitly spherical
  distance model, not ellipsoidal geodesy. Antimeridian differences wrap; poles
  and equivalent +/-180 longitudes have zero distance. Antipodal roundoff is clamped.
- `geo::within(point, center, radius_m)` returns a boolean using inclusive
  `distance <= radius_m`. Radius must be finite and nonnegative. No hidden epsilon.
- Point inputs require exactly the `type` and `coordinates` keys, literal type
  `Point`, and exactly two valid coordinates. Extra members, altitude, other
  geometries, NULL and missing arguments are errors. This intentionally accepts
  a narrower shape than arbitrary GeoJSON, and does not claim full GeoJSON support.
- These deterministic scalar functions work through existing expression lowering
  and object-write evaluation. Values use existing object encoding, clients and
  transfer formats, with no new storage tag or catalog version.
- Ordinary stored objects are not automatically spatially validated. The functions
  validate on use; a spatial index validates every supported write to its path.
  Scalar filtering may scan; use `search::near` for explicit indexed search.

```sql
INSERT INTO places {id: places:office, location: geo::point(106.7, 10.8)};
SELECT id, geo::distance(location, geo::point(106.71, 10.8)) AS distance_m
FROM places
WHERE geo::within(location, geo::point(106.71, 10.8), 5000)
ORDER BY distance_m, id;
```

## Managed spatial indexes and radius search

```sql
CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL;
SELECT id, distance_m
FROM search::near('places_location', geo::point(106.7, 10.8), 5000)
ORDER BY distance_m, id
LIMIT 20;
```

One index covers one point-valued field path in a collection. Names share the
managed-index namespace. `IF NOT EXISTS` is accepted only for an existing spatial
index on the same collection/path. `DROP INDEX name` removes it atomically;
`INFO FOR INDEX name` reports `kind: 'spatial'`. Native relational indexes remain
unchanged. `Connection::create_spatial_index` exposes the same operation in Rust.

Creating the index validates all existing point values and builds its entries
before publishing metadata. Missing and null fields are allowed and never match
a radius search. All other values must meet the strict point contract above.
Every supported insert, replacement, update, merge, upsert, transfer and migration
uses the common candidate validation/index maintenance path. Failed operations,
interruption and caller rollback preserve document/index agreement. Dropping a
collection removes its index storage. Reopen and integrity auditing validate the
spatial storage schema and both coordinate values in every entry.

`search::near(index_name, center, radius_m)` returns two columns: typed record `id`
and numeric `distance_m`. Inputs are literals, bound parameters, parentheses,
numeric unary signs or `geo::point` constructors. They are validated once during
preparation; correlated row arguments, arbitrary functions and subqueries are
not supported. Index names are data, never interpolated as SQL. Unknown indexes,
scalar indexes, invalid points/radii and wrong arity fail without scan fallback.

The source composes with SELECT projection, filters, joins, CTEs and LIMIT/OFFSET.
Use `ORDER BY distance_m,id` for deterministic distance/tie order; like ordinary
SQL, omitting ORDER BY does not promise order. LIMIT applies after distance
refinement. Search uses the statement snapshot, including pending transaction
writes, and the usual engine interruption and result-limit paths.

## Candidate index and distance model

The persisted index is a native B-tree ordered by latitude. Its protected entry
table also retains longitude and record ID; an optional field has a null entry
for auditing. Search forces a range on that index, then applies the same
spherical distance predicate used by `geo::within` to every candidate. It does
not load collection documents before filtering. See EXPLAIN QUERY PLAN and query
metrics for the actual execution path.

Latitude differs by at most the spherical central angle. Bounds are widened in
haversine space (64 binary64 epsilons), then outwards by 1e-9 degrees, to cover
roundoff, including the inverse formula near antipodes. This widening only admits
extra candidates; the radius comparison remains inclusive with no epsilon.
Longitude wrap and poles are handled in exact refinement, so no cell covering or
antimeridian splitting can omit candidates. This is spherical, not ellipsoidal,
accuracy.

Latitude ranges can be unselective when many points share a latitude or a radius
covers most of the globe. Such searches may read most index entries. This is an
explicit limited spatial index, not an R-tree/H3 or general geometry promise.
Cell aggregation uses the separate H3 grid described below.

## H3 cell aggregation

```sql
SELECT geo::cell(location, 7) AS cell, count(*) AS count
FROM places
GROUP BY geo::cell(location, 7)
ORDER BY cell;

SELECT geo::cell_center('822d57fffffffff');
```

`geo::cell(point, resolution)` returns a canonical lowercase hexadecimal H3 cell
address as a string. Resolution must be an integral finite number from 0 to 15;
integer-valued floats are accepted, strings and NULL are rejected. Points use the
same strict shape and coordinate validation as the other geo functions. Equivalent
+/-180 longitudes and coincident poles produce the same cell at every resolution.

`geo::cell_center(cell)` returns the cell's center as an ordinary Point object in
longitude/latitude order. The address must be a valid H3 cell in canonical lowercase
form; uppercase, leading zeroes, non-cell modes and malformed addresses reject.
The center is H3's cell center, not a promise of an ellipsoidal or area centroid.

The grid is pinned to h3o 0.9.4 with its libm implementation, using H3's spherical
icosahedral hierarchy and boundary assignment. Cells vary in area and include
pentagons. Logical parent relationships do not imply exact geometric containment.

Use ordinary GROUP BY, aggregate functions and HAVING for aggregation. Resolution
is explicit; there is no implicit rebucketing. Cell IDs can be stored and scalar
indexed as strings, transferred and reopened with the existing value format.
Computing a cell does not automatically create an index, and grouping may scan.
These cells do not filter radius candidates; `search::near` retains its documented
latitude index and distance refinement. No H3-neighbor radius-covering claim is made.

## Catalog compatibility

Collections acquiring spatial indexes are written at catalog version 3. Versions
1 and 2 remain readable, and scalar-only collections keep their existing version
(upgraded from 1 to 2 when ordinary metadata writes require it). Version 3 remains
on a collection after its last spatial index is dropped. V1 readers reject version
3; keep a pre-upgrade backup if rollback to V1 binaries is needed. Point values
still use the existing object encoding, without a new value/wire tag.
