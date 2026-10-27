//! Two-dimensional point functions, H3 cells and managed radius-search indexes.
mod cells;
#[cfg(test)]
mod evaluation;
mod exact;
mod geometry;
mod morton;
mod planner;

use crate::{Document, Error, Result, Value};
use serde::{Deserialize, Serialize};

const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Collections holding a two-dimensional spatial index use this catalog version.
pub(crate) const CATALOG_VERSION: u32 = 7;
/// Morton-keyed entries with longitude and latitude. Indexes without a
/// configuration use the released latitude-ordered format.
const MORTON_FORMAT: u32 = 2;
/// Upper bound on key ranges per search window set. Larger areas use coarser
/// ranges and read extra entries rather than issuing more seeks.
const RANGE_BUDGET: usize = 64;
/// Each range becomes one compiled UNION ALL arm. Merging the closest ranges
/// down to this count reads a few more nearby entries but compiles faster.
const STATEMENT_ARMS: usize = 8;
/// Nearest search starts at this radius and grows until `k` rows qualify.
const NEAREST_FIRST_RADIUS_M: f64 = 100.0;
pub(crate) const MAX_NEAREST: usize = 10_000;
const NEAREST_GLOBAL_JUMP_M: f64 = 2_000_000.0;

/// A managed table or index owned by an index, checked on open and audit.
pub(crate) struct SchemaObject {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) table: String,
    pub(crate) ddl: String,
}

/// Spatial work in the current top-level statement, for query profiles.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Counters {
    pub(crate) ranges: u64,
    pub(crate) rounds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Config {
    pub(crate) format: u32,
    /// Index Polygon and MultiPolygon values by covering cells, for finding
    /// the stored regions that contain a point.
    #[serde(default, skip_serializing_if = "crate::field_rules::is_false")]
    pub(crate) regions: bool,
}

impl Config {
    pub(crate) fn current() -> Self {
        Self {
            format: MORTON_FORMAT,
            regions: false,
        }
    }
    pub(crate) fn regions() -> Self {
        Self {
            format: MORTON_FORMAT,
            regions: true,
        }
    }
}

/// Covering cells stored per region. More cells prune more candidates for
/// small regions at the cost of more entries per document.
const REGION_CELLS: usize = 32;

pub(crate) fn validate_version(collection: &crate::Collection) -> Result<()> {
    for index in &collection.indexes {
        let Some(config) = &index.spatial else {
            continue;
        };
        if index.kind != crate::IndexKind::Spatial || config.format != MORTON_FORMAT {
            return Err(Error::Storage(format!(
                "unsupported spatial index format in {}",
                index.name
            )));
        }
        if collection.version < CATALOG_VERSION {
            return Err(Error::Storage(format!(
                "two-dimensional spatial indexes require catalog version {CATALOG_VERSION}"
            )));
        }
    }
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::Validation(format!("geo: {message}"))
}

fn number(value: &Value) -> Result<f64> {
    let value = match value {
        Value::Integer(n) => *n as f64,
        Value::Number(n) => *n,
        _ => return Err(invalid("expected finite number")),
    };
    if !value.is_finite() {
        return Err(invalid("expected finite number"));
    }
    Ok(value)
}

fn coordinates(longitude: &Value, latitude: &Value) -> Result<(f64, f64)> {
    let longitude = number(longitude)?;
    let latitude = number(latitude)?;
    if !(-180.0..=180.0).contains(&longitude) || !(-90.0..=90.0).contains(&latitude) {
        return Err(invalid(
            "longitude must be in [-180,180] and latitude in [-90,90]",
        ));
    }
    Ok((longitude, latitude))
}

fn point(value: &Value) -> Result<(f64, f64)> {
    let Value::Object(fields) = value else {
        return Err(invalid("expected Point object"));
    };
    if fields.len() != 2
        || !matches!(fields.get("type"), Some(Value::String(kind)) if kind == "Point")
    {
        return Err(invalid("expected only Point type and coordinates"));
    }
    let Some(Value::Array(values)) = fields.get("coordinates") else {
        return Err(invalid("expected coordinate array"));
    };
    let [longitude, latitude] = values.as_slice() else {
        return Err(invalid("expected two coordinates"));
    };
    coordinates(longitude, latitude)
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    // Normalize the difference, not stored values. Preserve exact zero for
    // equivalent antimeridian coordinates and coincident poles.
    let longitude = b.0 - a.0;
    let longitude = if longitude > 180.0 {
        longitude - 360.0
    } else if longitude < -180.0 {
        longitude + 360.0
    } else {
        longitude
    };
    if a.1 == b.1 && (longitude == 0.0 || a.1.abs() == 90.0) {
        return 0.0;
    }
    let latitude = (b.1 - a.1).to_radians();
    let cosine = |lat: f64| {
        if lat.abs() == 90.0 {
            0.0
        } else {
            lat.to_radians().cos()
        }
    };
    let h = (latitude / 2.0).sin().powi(2)
        + cosine(a.1) * cosine(b.1) * (longitude.to_radians() / 2.0).sin().powi(2);
    let h = h.clamp(0.0, 1.0);
    2.0 * EARTH_RADIUS_M * h.sqrt().atan2((1.0 - h).sqrt())
}

/// Public spellings, internal helper names and the operation each one runs.
/// Native `geo::*` names are canonical; `ST_*` names are PostGIS-style aliases.
const FUNCTIONS: &[(&str, &str, &str)] = &[
    ("geo::point", "geo_point", "point"),
    ("geo::distance", "geo_distance", "distance"),
    ("geo::within", "geo_within", "within"),
    ("geo::cell", "geo_cell", "cell"),
    ("geo::cell_center", "geo_cell_center", "cell_center"),
    ("geo::lng", "geo_lng", "lng"),
    ("geo::lat", "geo_lat", "lat"),
    ("geo::from_geojson", "geo_from_geojson", "from_geojson"),
    ("geo::to_geojson", "geo_to_geojson", "to_geojson"),
    ("geo::envelope", "geo_envelope", "envelope"),
    ("geo::contains", "geo_contains", "contains"),
    ("geo::covers", "geo_covers", "covers"),
    ("geo::inside", "geo_inside", "inside"),
    ("geo::intersects", "geo_intersects", "intersects"),
    ("geo::cell_boundary", "geo_cell_boundary", "cell_boundary"),
    ("geo::cell_parent", "geo_cell_parent", "cell_parent"),
    ("geo::cell_children", "geo_cell_children", "cell_children"),
    ("geo::cell_disk", "geo_cell_disk", "cell_disk"),
    (
        "geo::cell_resolution",
        "geo_cell_resolution",
        "cell_resolution",
    ),
    ("geo::cell_distance", "geo_cell_distance", "cell_distance"),
    ("geo::cell_area", "geo_cell_area", "cell_area"),
    ("geo::cell_compact", "geo_cell_compact", "cell_compact"),
    (
        "geo::cell_uncompact",
        "geo_cell_uncompact",
        "cell_uncompact",
    ),
    ("ST_Point", "st_point", "point"),
    ("ST_MakePoint", "st_makepoint", "point"),
    ("ST_X", "st_x", "lng"),
    ("ST_Y", "st_y", "lat"),
    ("ST_GeomFromGeoJSON", "st_geomfromgeojson", "from_geojson"),
    ("ST_AsGeoJSON", "st_asgeojson", "to_geojson"),
    ("ST_MakeEnvelope", "st_makeenvelope", "envelope"),
    ("ST_DistanceSphere", "st_distancesphere", "distance"),
    ("ST_DWithin", "st_dwithin", "within"),
    ("ST_Contains", "st_contains", "contains"),
    ("ST_Covers", "st_covers", "covers"),
    ("ST_Within", "st_within", "inside"),
    ("ST_Intersects", "st_intersects", "intersects"),
];

/// Released functions keep their strict NULL and error behavior.
const LEGACY: [&str; 5] = [
    "geo_point",
    "geo_distance",
    "geo_within",
    "geo_cell",
    "geo_cell_center",
];

const WGS84: f64 = 4326.0;
const PARSED_ENTRIES: usize = 8;
const PARSED_BYTES: usize = 8 << 20;
/// Prepared regions, keyed by their encoded bytes. Stored zones evaluated row
/// by row stay prepared across statements on the same thread.
const PREPARED_ENTRIES: usize = 4096;
const PREPARED_BYTES: usize = 32 << 20;
const REGISTERED_REGIONS: usize = 16;

type Shared<T> = std::rc::Rc<T>;
/// Least-recently-used regions: entry bytes, region and last use.
#[derive(Default)]
struct PreparedCache {
    entries: std::collections::HashMap<Vec<u8>, (Shared<geometry::Region>, u64)>,
    clock: u64,
    bytes: usize,
}

impl PreparedCache {
    fn get(&mut self, key: &[u8]) -> Option<Shared<geometry::Region>> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|(region, used)| {
            *used = clock;
            region.clone()
        })
    }
    fn insert(&mut self, key: Vec<u8>, region: Shared<geometry::Region>) {
        if key.len() > PREPARED_BYTES {
            return;
        }
        self.clock += 1;
        self.bytes += key.len();
        if self
            .entries
            .insert(key.clone(), (region, self.clock))
            .is_some()
        {
            self.bytes -= key.len();
        }
        while self.entries.len() > PREPARED_ENTRIES || self.bytes > PREPARED_BYTES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
                .expect("nonempty cache");
            self.bytes -= oldest.len();
            self.entries.remove(&oldest);
        }
    }
}

/// Next token and the registered regions, oldest first.
type Registry = (
    u64,
    std::collections::VecDeque<(u64, Shared<geometry::Region>)>,
);

thread_local! {
    static PREPARED: std::cell::RefCell<PreparedCache> = std::cell::RefCell::new(PreparedCache::default());
    static PARSED: std::cell::RefCell<Vec<(String, Value)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static REGISTERED: std::cell::RefCell<Registry> =
        const { std::cell::RefCell::new((0, std::collections::VecDeque::new())) };
}

/// Internal helper name for a public spelling, matched without case.
pub(crate) fn helper_name(public: &str) -> Option<&'static str> {
    FUNCTIONS
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(public))
        .map(|(_, helper, _)| *helper)
}

/// Static helper name and operation of an internal helper name.
fn operation(helper: &str) -> Option<(&'static str, &'static str)> {
    FUNCTIONS
        .iter()
        .find(|(_, name, _)| *name == helper)
        .map(|(_, name, operation)| (*name, *operation))
}

/// Public spelling of an internal helper name.
pub(crate) fn public_name(helper: &str) -> Option<&'static str> {
    FUNCTIONS
        .iter()
        .find(|(_, name, _)| *name == helper)
        .map(|(public, _, _)| *public)
}

/// Whether bare `ST_*` text names a supported alias.
pub(crate) fn alias_helper(word: &str) -> Option<&'static str> {
    if word.len() > 3 && word[..3].eq_ignore_ascii_case("st_") {
        helper_name(word)
    } else {
        None
    }
}

/// Whether a helper reports validation and limit errors with their own codes.
pub(crate) fn categorized_errors(helper: &str) -> bool {
    !LEGACY.contains(&helper) && FUNCTIONS.iter().any(|(_, name, _)| *name == helper)
}

pub(crate) fn call(helper: &str, args: &[Value]) -> Result<Value> {
    let Some(&(_, _, operation)) = FUNCTIONS.iter().find(|(_, name, _)| *name == helper) else {
        return Err(invalid("invalid function or argument count"));
    };
    if LEGACY.contains(&helper) {
        return legacy(operation, args);
    }
    if args.iter().any(|arg| matches!(arg, Value::Null)) {
        return Ok(Value::Null);
    }
    let alias = helper.starts_with("st_");
    match (operation, args) {
        ("point", [longitude, latitude]) => point_value(longitude, latitude),
        ("point", [longitude, latitude, srid]) if helper == "st_point" => {
            check_srid(srid)?;
            point_value(longitude, latitude)
        }
        ("lng", [value]) => Ok(Value::Number(point(value)?.0)),
        ("lat", [value]) => Ok(Value::Number(point(value)?.1)),
        ("distance", [_, _]) => legacy("distance", args),
        ("within", [_, _, _]) => legacy("within", args),
        ("within", [a, b, radius, spheroid]) if alias => {
            let spheroid = match spheroid {
                Value::Boolean(flag) => *flag,
                Value::Integer(flag @ (0 | 1)) => *flag == 1,
                _ => return Err(invalid("use_spheroid must be a boolean")),
            };
            if spheroid {
                return Err(invalid(
                    "spheroidal distance is not supported; pass use_spheroid => false",
                ));
            }
            legacy("within", &[a.clone(), b.clone(), radius.clone()])
        }
        ("from_geojson", [Value::String(text)]) => parsed_geojson(text),
        ("from_geojson", [_]) => Err(invalid("expected GeoJSON text")),
        ("to_geojson", [value]) => Ok(Value::String(
            geometry::Geometry::from_value(value)?.to_geojson()?,
        )),
        ("envelope", [west, south, east, north]) => envelope(west, south, east, north),
        ("envelope", [west, south, east, north, srid]) if alias => {
            check_srid(srid)?;
            envelope(west, south, east, north)
        }
        ("contains", [region, p]) | ("inside", [p, region]) => Ok(Value::Boolean(
            prepared(region)?.locate(point(p)?) == geometry::Location::Inside,
        )),
        ("covers", [region, p]) => Ok(Value::Boolean(
            prepared(region)?.locate(point(p)?) != geometry::Location::Outside,
        )),
        ("intersects", [a, b]) => {
            let (region, p) = match (is_point(a), is_point(b)) {
                (true, false) => (b, a),
                (false, true) => (a, b),
                _ => {
                    return Err(invalid(
                        "intersects supports one Point and one Polygon or MultiPolygon",
                    ))
                }
            };
            Ok(Value::Boolean(
                prepared(region)?.locate(point(p)?) != geometry::Location::Outside,
            ))
        }
        (operation, _) if operation.starts_with("cell_") => cells::call(operation, args),
        _ => Err(invalid("invalid function or argument count")),
    }
}

fn legacy(operation: &str, args: &[Value]) -> Result<Value> {
    match (operation, args) {
        ("point", [longitude, latitude]) => point_value(longitude, latitude),
        ("cell", [value, level]) => {
            let (mut longitude, latitude) = point(value)?;
            // Equivalent geographic positions must get the same cell even at
            // fine resolutions near poles and the antimeridian.
            if latitude.abs() == 90.0 {
                longitude = 0.0;
            }
            if longitude == -180.0 {
                longitude = 180.0;
            }
            let resolution = resolution(level)?;
            let coordinate = h3o::LatLng::new(latitude, longitude)
                .map_err(|_| invalid("invalid cell coordinates"))?;
            Ok(Value::String(coordinate.to_cell(resolution).to_string()))
        }
        ("cell_center", [address @ Value::String(_)]) => {
            let center = h3o::LatLng::from(cells::parse(address)?);
            point_value(&Value::Number(center.lng()), &Value::Number(center.lat()))
        }
        ("distance", [a, b]) => Ok(Value::Number(distance(point(a)?, point(b)?))),
        ("within", [a, b, radius]) => {
            let a = point(a)?;
            let b = point(b)?;
            let radius = number(radius)?;
            if radius < 0.0 {
                return Err(invalid("radius must be nonnegative"));
            }
            Ok(Value::Boolean(distance(a, b) <= radius))
        }
        _ => Err(invalid("invalid function or argument count")),
    }
}

fn point_value(longitude: &Value, latitude: &Value) -> Result<Value> {
    let (longitude, latitude) = coordinates(longitude, latitude)?;
    Ok(Value::Object(Document::from([
        ("type".into(), Value::String("Point".into())),
        (
            "coordinates".into(),
            Value::Array(vec![Value::Number(longitude), Value::Number(latitude)]),
        ),
    ])))
}

fn resolution(value: &Value) -> Result<h3o::Resolution> {
    let resolution = number(value)?;
    if !(0.0..=15.0).contains(&resolution) || resolution.fract() != 0.0 {
        return Err(invalid(
            "cell resolution must be an integer from 0 through 15",
        ));
    }
    h3o::Resolution::try_from(resolution as u8).map_err(|_| invalid("invalid cell resolution"))
}

fn check_srid(srid: &Value) -> Result<()> {
    if number(srid)? != WGS84 {
        return Err(invalid(
            "only SRID 4326 (WGS84 longitude/latitude) is supported",
        ));
    }
    Ok(())
}

fn envelope(west: &Value, south: &Value, east: &Value, north: &Value) -> Result<Value> {
    let (west, south) = coordinates(west, south)?;
    let (east, north) = coordinates(east, north)?;
    if !(west < east && east - west < 180.0 && -90.0 < south && south < north && north < 90.0) {
        return Err(invalid(
            "envelope requires west < east, a width under 180 degrees and -90 < south < north < 90; use search::bbox for wider or dateline-crossing viewports",
        ));
    }
    let ring = vec![
        (west, south),
        (east, south),
        (east, north),
        (west, north),
        (west, south),
    ];
    Ok(geometry::Region::new(geometry::Kind::Polygon, vec![vec![ring]])?.to_value())
}

fn is_point(value: &Value) -> bool {
    matches!(value, Value::Object(fields) if matches!(fields.get("type"), Some(Value::String(kind)) if kind == "Point"))
}

/// Parse GeoJSON once while the same text repeats across the rows of a statement.
fn parsed_geojson(text: &str) -> Result<Value> {
    let cached = PARSED.with(|cache| {
        let cache = cache.borrow();
        cache
            .iter()
            .find(|(key, _)| key == text)
            .map(|(_, value)| value.clone())
    });
    if let Some(value) = cached {
        return Ok(value);
    }
    let value = geometry::Geometry::from_geojson(text)?.to_value();
    if text.len() <= PARSED_BYTES {
        PARSED.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.insert(0, (text.to_owned(), value.clone()));
            cache.truncate(PARSED_ENTRIES);
            while cache.iter().map(|(key, _)| key.len()).sum::<usize>() > PARSED_BYTES {
                cache.pop();
            }
        });
    }
    Ok(value)
}

/// Hold a prepared region for the statement being lowered. Generated SQL refers
/// to it by token, so refinement never copies the region per candidate row.
/// Lowering and execution run on one thread within one call, and the newest
/// registrations are retained.
fn register_region(region: Shared<geometry::Region>) -> u64 {
    REGISTERED.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.0 += 1;
        let token = registry.0;
        registry.1.push_back((token, region));
        while registry.1.len() > REGISTERED_REGIONS {
            registry.1.pop_front();
        }
        token
    })
}

/// Location of a stored point in a registered region: 0 outside, 1 on the
/// boundary, 2 inside.
pub(crate) fn locate_registered(token: i64, point: (f64, f64)) -> Result<i64> {
    let region = REGISTERED.with(|registry| {
        registry
            .borrow()
            .1
            .iter()
            .find(|(key, _)| *key as i64 == token)
            .map(|(_, region)| region.clone())
    });
    let region = region.ok_or_else(|| {
        Error::Storage(
            "spatial query region is no longer registered; run the statement again".into(),
        )
    })?;
    Ok(match region.locate(point) {
        geometry::Location::Outside => 0,
        geometry::Location::Boundary => 1,
        geometry::Location::Inside => 2,
    })
}

/// Location of a point in a stored canonical region: 0 outside, 1 on the
/// boundary, 2 inside. The stored bytes are the cache key.
pub(crate) fn locate_shape(bytes: &[u8], point: (f64, f64)) -> Result<i64> {
    let region = match PREPARED.with(|cache| cache.borrow_mut().get(bytes)) {
        Some(region) => region,
        None => {
            let geometry::Geometry::Region(region) =
                geometry::Geometry::from_value(&Value::decode(bytes)?)?
            else {
                return Err(Error::Storage("stored region shape is not a region".into()));
            };
            let region = Shared::new(region);
            PREPARED.with(|cache| cache.borrow_mut().insert(bytes.to_vec(), region.clone()));
            region
        }
    };
    Ok(match region.locate(point) {
        geometry::Location::Outside => 0,
        geometry::Location::Boundary => 1,
        geometry::Location::Inside => 2,
    })
}

/// Validate a region once and reuse it while the same encoded value repeats,
/// as a constant query region does across the rows of one statement.
fn prepared(value: &Value) -> Result<Shared<geometry::Region>> {
    if is_point(value) {
        return Err(invalid("expected a Polygon or MultiPolygon region"));
    }
    let key = value.encode()?;
    if let Some(region) = PREPARED.with(|cache| cache.borrow_mut().get(&key)) {
        return Ok(region);
    }
    let geometry::Geometry::Region(region) = geometry::Geometry::from_value(value)? else {
        return Err(invalid("expected a Polygon or MultiPolygon region"));
    };
    let region = Shared::new(region);
    PREPARED.with(|cache| cache.borrow_mut().insert(key, region.clone()));
    Ok(region)
}

impl crate::Index {
    pub(crate) fn table_ddl(&self) -> String {
        if self.kind == crate::IndexKind::Array {
            return format!(
                "CREATE TABLE {} (\"key\" BLOB NOT NULL,id BLOB NOT NULL,PRIMARY KEY(id,\"key\"))",
                crate::quote(&self.storage)
            );
        }
        if self.kind == crate::IndexKind::FullText {
            return self.text_table_ddl();
        }
        if self.region_index() {
            return format!(
                "CREATE TABLE {} (\"key\" INTEGER NOT NULL, id BLOB NOT NULL, PRIMARY KEY(id, \"key\"))",
                crate::quote(&self.storage)
            );
        }
        let extra = if self.kind == crate::IndexKind::Spatial && self.spatial.is_some() {
            ", longitude, latitude".to_owned()
        } else if self.kind == crate::IndexKind::Spatial {
            ", longitude".to_owned()
        } else if self.kind == crate::IndexKind::Scalar {
            (1..self.paths().count())
                .map(|i| format!(",f{i}"))
                .collect::<String>()
        } else {
            String::new()
        };
        format!(
            "CREATE TABLE {} (\"key\", id BLOB NOT NULL{extra})",
            crate::quote(&self.storage)
        )
    }
    pub(crate) fn index_ddl(&self) -> String {
        if self.kind == crate::IndexKind::Array {
            return format!(
                "CREATE INDEX {} ON {} (\"key\",id)",
                crate::quote(&self.name),
                crate::quote(&self.storage)
            );
        }
        if self.kind == crate::IndexKind::Vector {
            return format!(
                "CREATE UNIQUE INDEX {} ON {} (id)",
                crate::quote(&self.name),
                crate::quote(&self.storage)
            );
        }
        if self.kind == crate::IndexKind::FullText {
            return self.text_index_ddl();
        }
        if self.kind == crate::IndexKind::Scalar {
            return format!(
                "CREATE {} INDEX {} ON {} ({})",
                if self.unique { "UNIQUE" } else { "" },
                crate::quote(&self.name),
                crate::quote(&self.storage),
                self.scalar_columns().join(",")
            );
        }
        if self.region_index() {
            return format!(
                "CREATE INDEX {} ON {} (\"key\", id)",
                crate::quote(&self.name),
                crate::quote(&self.storage)
            );
        }
        // Spatial entries include nulls for optional fields. Radius predicates
        // exclude them while integrity audits retain one entry per document.
        // Two-dimensional entries carry their coordinates and ID in the index,
        // so range scans refine candidates without reading the table row.
        if self.spatial.is_some() {
            return format!(
                "CREATE INDEX {} ON {} (\"key\", longitude, latitude, id)",
                crate::quote(&self.name),
                crate::quote(&self.storage)
            );
        }
        format!(
            "CREATE {} INDEX {} ON {} (\"key\")",
            if self.unique { "UNIQUE" } else { "" },
            crate::quote(&self.name),
            crate::quote(&self.storage)
        )
    }
    /// Scalar and spatial indexes hold one entry per document, findable by its
    /// key columns, so writes maintain them by key instead of by ID.
    pub(crate) fn keyed_maintenance(&self) -> bool {
        self.kind == crate::IndexKind::Scalar
            || (self.kind == crate::IndexKind::Spatial && !self.region_index())
    }

    pub(crate) fn region_index(&self) -> bool {
        self.kind == crate::IndexKind::Spatial && self.spatial.as_ref().is_some_and(|c| c.regions)
    }

    /// Canonical region shapes of a region index, keyed by record ID, so
    /// refinement reads one small value instead of the whole document.
    pub(crate) fn region_shapes(&self) -> String {
        format!("{}_shapes", self.storage)
    }

    /// Managed schema objects beyond the entry table and its search index.
    pub(crate) fn extra_schema(&self) -> Vec<SchemaObject> {
        if !self.region_index() {
            return Vec::new();
        }
        let name = self.region_shapes();
        vec![SchemaObject {
            ddl: format!(
                "CREATE TABLE {} (id BLOB PRIMARY KEY, shape BLOB NOT NULL)",
                crate::quote(&name)
            ),
            table: name.clone(),
            name,
            kind: "table",
        }]
    }

    /// The region a document stores at the indexed path, if any.
    fn stored_region(&self, document: &Document) -> Result<Option<geometry::Region>> {
        match crate::path_value(document, &self.path)? {
            None | Some(Value::Null) => Ok(None),
            Some(value) => match geometry::Geometry::from_value(value)? {
                geometry::Geometry::Region(region) => Ok(Some(region)),
                geometry::Geometry::Point(_) => Err(invalid(
                    "a region index requires Polygon or MultiPolygon values",
                )),
            },
        }
    }

    pub(crate) fn keys(&self, value: &Value) -> Result<Vec<turso_core::Value>> {
        if self.kind == crate::IndexKind::Vector {
            return self.vector_keys(value);
        }
        if self.kind == crate::IndexKind::Scalar {
            return Ok(vec![crate::index_scalar(value)?]);
        }
        if self.region_index() {
            if !matches!(value, Value::Null) {
                let geometry::Geometry::Region(_) = geometry::Geometry::from_value(value)? else {
                    return Err(invalid(
                        "a region index requires Polygon or MultiPolygon values",
                    ));
                };
            }
            return Ok(Vec::new());
        }
        let two_dimensional = self.spatial.is_some();
        if matches!(value, Value::Null) {
            let columns = if two_dimensional { 3 } else { 2 };
            return Ok(vec![turso_core::Value::Null; columns]);
        }
        let (longitude, latitude) = point(value)?;
        if two_dimensional {
            return Ok(vec![
                crate::scalar(&Value::Integer(morton::key((longitude, latitude))))?,
                crate::scalar(&Value::Number(longitude))?,
                crate::scalar(&Value::Number(latitude))?,
            ]);
        }
        Ok(vec![
            crate::scalar(&Value::Number(latitude))?,
            crate::scalar(&Value::Number(longitude))?,
        ])
    }
}

/// Evaluate only immutable inputs, once per preparation; never execute SQL or callbacks.
pub(crate) fn search_argument(
    expr: &turso_parser::ast::Expr,
    params: &crate::Parameters,
    consumed: &mut std::collections::BTreeSet<String>,
) -> Result<Value> {
    use turso_parser::ast::{Expr, Literal, UnaryOperator};
    match expr {
        Expr::Literal(Literal::Numeric(n)) => {
            let number = n
                .parse::<f64>()
                .map_err(|_| invalid("expected decimal number"))?;
            if !number.is_finite() {
                return Err(invalid("expected finite number"));
            }
            Ok(Value::Number(number))
        }
        Expr::Literal(Literal::String(s)) => {
            let tokens = romeql_parser::tokenize(s)?;
            match tokens.as_slice() {
                [token] if token.kind == romeql_parser::Kind::String => {
                    Ok(Value::String(token.text.clone()))
                }
                _ => Err(invalid("expected string literal")),
            }
        }
        Expr::Variable(v) => {
            let name = v
                .name
                .as_ref()
                .map_or_else(|| format!("?{}", v.index), |name| name.to_string());
            consumed.insert(name.clone());
            let value = params
                .get(&name)
                .cloned()
                .ok_or(crate::Error::Parameter(name))?;
            value.validate()?;
            Ok(value)
        }
        Expr::Parenthesized(es) if es.len() == 1 => search_argument(&es[0], params, consumed),
        Expr::Unary(op @ (UnaryOperator::Positive | UnaryOperator::Negative), e) => {
            let value = number(&search_argument(e, params, consumed)?)?;
            Ok(Value::Number(if *op == UnaryOperator::Negative {
                -value
            } else {
                value
            }))
        }
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            order_by,
            within_group,
            filter_over,
        } if (name.as_str() == "vector32" || search_constructor(name.as_str()).is_some())
            && distinctness.is_none()
            && order_by.is_empty()
            && within_group.is_empty()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none() =>
        {
            let args = args
                .iter()
                .map(|arg| search_argument(arg, params, consumed))
                .collect::<Result<Vec<_>>>()?;
            if name.as_str() == "vector32" {
                let [Value::String(input)] = args.as_slice() else {
                    return Err(invalid(
                        "vector32 search constructor requires a JSON array string",
                    ));
                };
                let values: Vec<f32> = serde_json::from_str(input)
                    .map_err(|_| invalid("invalid vector32 JSON array"))?;
                Value::vector32(&values)
            } else {
                call(
                    search_constructor(name.as_str()).expect("constructor"),
                    &args,
                )
            }
        }
        _ => Err(invalid(
            "search arguments must be literals, parameters, geometry constructors or vector32",
        )),
    }
}

fn search_constructor(name: &str) -> Option<&'static str> {
    let helper = name.strip_prefix("__romedb_h_")?;
    FUNCTIONS
        .iter()
        .find(|(_, candidate, operation)| {
            *candidate == helper && matches!(*operation, "point" | "from_geojson" | "envelope")
        })
        .map(|(_, helper, _)| *helper)
}

impl crate::Connection {
    pub(crate) fn spatial_search_sql(
        &self,
        index: &Value,
        center: &Value,
        radius: &Value,
    ) -> Result<String> {
        let Value::String(name) = index else {
            return Err(invalid("search index name must be a string"));
        };
        let name = crate::canonical(name)?;
        let center = point(center)?;
        let radius = number(radius)?;
        if radius < 0.0 {
            return Err(invalid("radius must be nonnegative"));
        }
        let index = self.spatial_index(&name, "search::near")?;
        if index.spatial.is_some() {
            let distance = format!(
                "__romedb_geo_distance(longitude, latitude, {}, {})",
                center.0, center.1
            );
            let ranges = morton::cover_windows(&radius_windows(center, radius), RANGE_BUDGET);
            self.record_spatial(ranges.len(), 0);
            return Ok(ranged_sql(
                &index,
                &ranges,
                &format!("id, {distance} AS distance_m"),
                &format!("{distance} <= {radius}"),
                "NULL AS id, NULL AS distance_m",
            ));
        }
        self.record_spatial(1, 0);
        // Latitude is 1-Lipschitz in central angle. Widen outward before the
        // index range comparison to cover floating point boundary rounding.
        // Refinement always applies the same distance predicate as geo::within.
        let (low, high) = latitude_bounds(center.1, radius);
        let distance = format!(
            "__romedb_geo_distance(longitude, key, {}, {})",
            center.0, center.1
        );
        Ok(format!("SELECT id, {distance} AS distance_m FROM {} INDEXED BY {} WHERE key >= {low} AND key <= {high} AND {distance} <= {radius}", crate::quote(&index.storage), crate::quote(&index.name)))
    }

    /// Points inside an inclusive viewport. `west > east` crosses the antimeridian.
    pub(crate) fn bbox_search_sql(&self, args: &[Value]) -> Result<String> {
        let [index, west, south, east, north] = args else {
            return Err(invalid(
                "search::bbox expects index name, west, south, east and north",
            ));
        };
        let Value::String(name) = index else {
            return Err(invalid("search index name must be a string"));
        };
        let name = crate::canonical(name)?;
        let (west, south) = coordinates(west, south)?;
        let (east, north) = coordinates(east, north)?;
        if south > north {
            return Err(invalid("viewport south must not exceed north"));
        }
        let index = self.spatial_index(&name, "search::bbox")?;
        let test = |longitude: &str, latitude: &str| {
            format!("__romedb_geo_bbox({longitude}, {latitude}, {west}, {south}, {east}, {north})")
        };
        if index.spatial.is_some() {
            let ranges =
                morton::cover_windows(&bbox_windows(west, south, east, north), RANGE_BUDGET);
            self.record_spatial(ranges.len(), 0);
            return Ok(ranged_sql(
                &index,
                &ranges,
                "id",
                &test("longitude", "latitude"),
                "NULL AS id",
            ));
        }
        self.record_spatial(1, 0);
        Ok(format!(
            "SELECT id FROM {} INDEXED BY {} WHERE key >= {south} AND key <= {north} AND {}",
            crate::quote(&index.storage),
            crate::quote(&index.name),
            test("longitude", "key")
        ))
    }

    /// Points that a region contains, or covers including its boundary.
    pub(crate) fn polygon_search_sql(&self, args: &[Value]) -> Result<String> {
        let [index, region, mode] = args else {
            return Err(invalid(
                "search::polygon expects index name, region and mode",
            ));
        };
        let Value::String(name) = index else {
            return Err(invalid("search index name must be a string"));
        };
        let name = crate::canonical(name)?;
        let region = prepared(region)?;
        let minimum = match mode {
            Value::String(mode) if mode == "contains" => 2,
            Value::String(mode) if mode == "covers" => 1,
            _ => {
                return Err(invalid(
                    "search::polygon mode must be 'contains' or 'covers'",
                ))
            }
        };
        let index = self.spatial_index(&name, "search::polygon")?;
        let bounds = region.bounds();
        let token = register_region(region.clone());
        let test = |longitude: &str, latitude: &str| {
            format!("__romedb_geo_locate({token}, {longitude}, {latitude}) >= {minimum}")
        };
        if index.spatial.is_some() {
            let mut ranges = morton::cover_region(&region, RANGE_BUDGET);
            let mut seams = Vec::new();
            if bounds.east >= 180.0 {
                seams.push(morton::Window::from_degrees(
                    -180.0,
                    bounds.south,
                    -180.0,
                    bounds.north,
                ));
            }
            if bounds.west <= -180.0 {
                seams.push(morton::Window::from_degrees(
                    180.0,
                    bounds.south,
                    180.0,
                    bounds.north,
                ));
            }
            if !seams.is_empty() {
                ranges.extend(morton::cover_windows(&seams, RANGE_BUDGET));
                ranges = morton::merge(ranges);
            }
            self.record_spatial(ranges.len(), 0);
            return Ok(ranged_sql(
                &index,
                &ranges,
                "id",
                &test("longitude", "latitude"),
                "NULL AS id",
            ));
        }
        self.record_spatial(1, 0);
        Ok(format!(
            "SELECT id FROM {} INDEXED BY {} WHERE key >= {} AND key <= {} AND {}",
            crate::quote(&index.storage),
            crate::quote(&index.name),
            bounds.south,
            bounds.north,
            test("longitude", "key")
        ))
    }

    /// Exact `k` nearest indexed points, ordered by distance and typed ID.
    ///
    /// Each round is a complete radius search. A round that returns `k` rows
    /// stops the search, because every unvisited point is farther than the
    /// round radius and the round radius is at least the k-th distance. The
    /// statement runs in one savepoint, so all rounds and the outer query read
    /// one snapshot.
    pub(crate) fn nearest_search_sql(
        &self,
        args: &[Value],
        allowed: Option<&Value>,
    ) -> Result<String> {
        let [index, center, k] = args else {
            return Err(invalid(
                "search::nearest expects index name, center point, k and optional allowed IDs",
            ));
        };
        let Value::String(name) = index else {
            return Err(invalid("search index name must be a string"));
        };
        let name = crate::canonical(name)?;
        let center = point(center)?;
        let k = number(k)?;
        if !(1.0..=MAX_NEAREST as f64).contains(&k) || k.fract() != 0.0 {
            return Err(invalid(&format!(
                "search::nearest k must be an integer from 1 to {MAX_NEAREST}"
            )));
        }
        let k = k as usize;
        let (table, index) = self.spatial_index_owner(&name, "search::nearest")?;
        let allowed = allowed
            .map(|ids| crate::search_filter::Filter::from_value(ids, &table))
            .transpose()?
            .map(|filter| format!(" AND {}", filter.predicate()))
            .unwrap_or_default();
        // EXPLAIN describes the first round's index search at its nominal radius
        // and reads no data; execution chooses later radii from the index.
        if crate::graph::explaining() {
            return Ok(self.nearest_round_sql(&index, center, k, &allowed, NEAREST_FIRST_RADIUS_M));
        }
        let (rows, _) = self.nearest_rows(&index, center, k, &allowed, NEAREST_FIRST_RADIUS_M)?;
        if rows.is_empty() {
            return Ok("SELECT NULL AS id, NULL AS distance_m WHERE 0".into());
        }
        let values = rows
            .iter()
            .map(|row| match row.as_slice() {
                [turso_core::Value::Blob(id), distance] => {
                    let distance = match distance {
                        turso_core::Value::Numeric(turso_core::Numeric::Float(d)) => f64::from(*d),
                        turso_core::Value::Numeric(turso_core::Numeric::Integer(d)) => *d as f64,
                        _ => return Err(Error::Storage("invalid nearest distance".into())),
                    };
                    let id = id.iter().map(|b| format!("{b:02x}")).collect::<String>();
                    Ok(format!("(x'{id}',{distance:?})"))
                }
                _ => Err(Error::Storage("invalid nearest entry".into())),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "WITH __romedb_nearest_hits(id,distance_m) AS MATERIALIZED (VALUES {}) SELECT id,distance_m FROM __romedb_nearest_hits",
            values.join(",")
        ))
    }

    /// Complete radius rounds from `start` until one returns `k` rows or covers
    /// the globe. Returns the rows and the radius of the final round.
    pub(crate) fn nearest_rows(
        &self,
        index: &crate::Index,
        center: (f64, f64),
        k: usize,
        allowed: &str,
        start: f64,
    ) -> Result<(Vec<Vec<turso_core::Value>>, f64)> {
        let mut radius = start;
        loop {
            let last = radius >= std::f64::consts::PI * EARTH_RADIUS_M;
            if last {
                radius = f64::MAX;
            }
            let round = self.nearest_round_sql(index, center, k, allowed, radius);
            // Customer reads: a metered statement charges every round.
            let rows = self.run_customer(&round, &[])?;
            if rows.len() == k || last {
                return Ok((rows, radius));
            }
            radius = next_radius(radius, rows.len(), k);
        }
    }

    /// SQL for one nearest round: the `k` closest indexed points within `radius`.
    fn nearest_round_sql(
        &self,
        index: &crate::Index,
        center: (f64, f64),
        k: usize,
        allowed: &str,
        radius: f64,
    ) -> String {
        let candidates = if index.spatial.is_some() {
            let distance = format!(
                "__romedb_geo_distance(longitude, latitude, {}, {})",
                center.0, center.1
            );
            let ranges = morton::cover_windows(&radius_windows(center, radius), RANGE_BUDGET);
            self.record_spatial(ranges.len(), 1);
            ranged_sql(
                index,
                &ranges,
                &format!("id, {distance} AS distance_m"),
                &format!("{distance} <= {radius}{allowed}"),
                "NULL AS id, NULL AS distance_m",
            )
        } else {
            self.record_spatial(1, 1);
            let (low, high) = latitude_bounds(center.1, radius);
            let distance = format!(
                "__romedb_geo_distance(longitude, key, {}, {})",
                center.0, center.1
            );
            format!(
                    "SELECT id, {distance} AS distance_m FROM {} INDEXED BY {} WHERE key >= {low} AND key <= {high} AND {distance} <= {radius}{allowed}",
                    crate::quote(&index.storage),
                    crate::quote(&index.name)
                )
        };
        format!(
            "SELECT id, distance_m FROM ({candidates}) ORDER BY distance_m, __romedb_sort_encoded(id) LIMIT {k}"
        )
    }

    /// Statements started by a nearest search add to the outer profile, so the
    /// counters reset only when a top-level statement starts.
    pub(crate) fn reset_spatial_counters(&self) {
        *self
            .spatial_counters
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Counters::default();
    }

    pub(crate) fn spatial_counters(&self) -> Counters {
        *self
            .spatial_counters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn record_spatial(&self, ranges: usize, rounds: u64) {
        let mut counters = self
            .spatial_counters
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        counters.ranges += ranges.min(STATEMENT_ARMS) as u64;
        counters.rounds += rounds;
    }

    /// Store a document's region: its covering cells and its canonical shape.
    pub(crate) fn insert_region_entries(
        &self,
        index: &crate::Index,
        document: &Document,
    ) -> Result<()> {
        let Some(region) = index.stored_region(document)? else {
            return Ok(());
        };
        let id = turso_core::Value::Blob(document["id"].encode()?);
        let cells = format!(
            "INSERT INTO {}(\"key\", id) VALUES (?1, ?2)",
            crate::quote(&index.storage)
        );
        for cell in morton::region_cells(&region, REGION_CELLS) {
            self.run_index_maintenance(&cells, &[turso_core::Value::from_i64(cell), id.clone()])?;
        }
        self.run_index_maintenance(
            &format!(
                "INSERT INTO {}(id, shape) VALUES (?1, ?2)",
                crate::quote(&index.region_shapes())
            ),
            &[id, turso_core::Value::Blob(region.to_value().encode()?)],
        )?;
        Ok(())
    }

    pub(crate) fn delete_region_shape(
        &self,
        index: &crate::Index,
        id: &turso_core::Value,
    ) -> Result<()> {
        self.run_index_maintenance(
            &format!(
                "DELETE FROM {} WHERE id = ?1",
                crate::quote(&index.region_shapes())
            ),
            std::slice::from_ref(id),
        )?;
        Ok(())
    }

    /// Compare every document's expected cells and shape with the stored
    /// entries, and reject entries without a document. Returns the entry count.
    pub(crate) fn audit_region_index(
        &self,
        collection: &crate::Collection,
        index: &crate::Index,
    ) -> Result<u64> {
        let stored = |rows: Vec<Vec<turso_core::Value>>| -> Result<u64> {
            match rows.first().and_then(|row| row.first()) {
                Some(turso_core::Value::Numeric(turso_core::Numeric::Integer(n))) => Ok(*n as u64),
                _ => Err(Error::Storage("invalid region index count".into())),
            }
        };
        let mut cells_total = 0u64;
        let mut shapes_total = 0u64;
        for document in self.documents(collection)? {
            let Some(region) = index.stored_region(&document)? else {
                continue;
            };
            let id = turso_core::Value::Blob(document["id"].encode()?);
            let expected = morton::region_cells(&region, REGION_CELLS);
            let actual = self
                .run(
                    &format!(
                        "SELECT \"key\" FROM {} WHERE id = ?1 ORDER BY \"key\"",
                        crate::quote(&index.storage)
                    ),
                    std::slice::from_ref(&id),
                )?
                .into_iter()
                .map(|row| match row.as_slice() {
                    [turso_core::Value::Numeric(turso_core::Numeric::Integer(cell))] => Ok(*cell),
                    _ => Err(Error::Storage(format!(
                        "invalid entry in index {}",
                        index.name
                    ))),
                })
                .collect::<Result<Vec<_>>>()?;
            if actual != expected {
                return Err(Error::Storage(format!(
                    "stale entry in index {}",
                    index.name
                )));
            }
            let shape = self.run(
                &format!(
                    "SELECT shape FROM {} WHERE id = ?1",
                    crate::quote(&index.region_shapes())
                ),
                std::slice::from_ref(&id),
            )?;
            if shape != vec![vec![turso_core::Value::Blob(region.to_value().encode()?)]] {
                return Err(Error::Storage(format!(
                    "stale shape in index {}",
                    index.name
                )));
            }
            cells_total += expected.len() as u64;
            shapes_total += 1;
        }
        let count = |table: &str| -> Result<u64> {
            stored(self.run(
                &format!("SELECT count(*) FROM {}", crate::quote(table)),
                &[],
            )?)
        };
        if count(&index.storage)? != cells_total || count(&index.region_shapes())? != shapes_total {
            return Err(Error::Storage(format!(
                "orphan entry in index {}",
                index.name
            )));
        }
        Ok(cells_total)
    }

    /// Stored regions that contain, or cover including their boundary, a point.
    pub(crate) fn containing_search_sql(&self, args: &[Value]) -> Result<String> {
        let [index, center, mode] = args else {
            return Err(invalid(
                "search::containing expects index name, point and mode",
            ));
        };
        let Value::String(name) = index else {
            return Err(invalid("search index name must be a string"));
        };
        let name = crate::canonical(name)?;
        let center = point(center)?;
        let minimum = match mode {
            Value::String(mode) if mode == "contains" => 2,
            Value::String(mode) if mode == "covers" => 1,
            _ => {
                return Err(invalid(
                    "search::containing mode must be 'contains' or 'covers'",
                ))
            }
        };
        let index = self.spatial_index(&name, "search::containing")?;
        if !index.region_index() {
            return Err(invalid("search::containing requires a region index"));
        }
        let mut codes = morton::ancestor_codes(center);
        if center.0.abs() == 180.0 {
            codes.extend(morton::ancestor_codes((-center.0, center.1)));
        }
        codes.sort_unstable();
        codes.dedup();
        self.record_spatial(1, 0);
        let codes = codes
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // No INDEXED BY: the engine seeks an IN list through the key index only
        // when the planner chooses it, and scans the whole index when forced.
        Ok(format!(
            "SELECT id FROM {} WHERE id IN (SELECT id FROM {} WHERE \"key\" IN ({codes})) AND __romedb_geo_shape_locate(shape, {}, {}) >= {minimum}",
            crate::quote(&index.region_shapes()),
            crate::quote(&index.storage),
            center.0,
            center.1
        ))
    }

    fn spatial_index(&self, name: &str, source: &str) -> Result<crate::Index> {
        self.spatial_index_owner(name, source)
            .map(|(_, index)| index)
    }

    fn spatial_index_owner(&self, name: &str, source: &str) -> Result<(String, crate::Index)> {
        let (table, index) = self
            .collections()?
            .into_iter()
            .flat_map(|c| {
                let table = c.name.clone();
                c.indexes
                    .into_iter()
                    .map(move |index| (table.clone(), index))
            })
            .find(|(_, i)| i.name == name)
            .ok_or_else(|| crate::Error::NotFound(format!("spatial index {name}")))?;
        if index.kind != crate::IndexKind::Spatial {
            return Err(invalid(&format!("{source} requires a spatial index")));
        }
        if index.region_index() != (source == "search::containing") {
            let kind = if index.region_index() {
                "point"
            } else {
                "region"
            };
            return Err(invalid(&format!(
                "{source} requires a {kind} spatial index"
            )));
        }
        Ok((table, index))
    }
}

/// Grow a nearest radius. An empty round says nothing about density, so the
/// radius quadruples. Otherwise the area grows by about the shortfall, assuming
/// locally even density, at least doubling and at most growing eightfold.
pub(crate) fn next_radius(radius: f64, found: usize, k: usize) -> f64 {
    let next = if found == 0 {
        radius * 4.0
    } else {
        radius * ((k as f64 / found as f64).sqrt() * 1.25).clamp(2.0, 8.0)
    };
    // Beyond continental scale, more rounds only re-read most of the index;
    // one round over the whole globe is cheaper.
    if next >= NEAREST_GLOBAL_JUMP_M {
        std::f64::consts::PI * EARTH_RADIUS_M
    } else {
        next
    }
}

/// Whether a statement uses a source that reads the index before the statement
/// runs; such statements execute in one savepoint so every read shares a snapshot.
///
/// Ordinary nearest statements probe the index before running, so any SELECT
/// that orders by a spatial distance and has a LIMIT is also included.
pub(crate) fn has_snapshot_sources(sql: &str) -> Result<bool> {
    let tokens = romeql_parser::tokenize(sql)?;
    let source = tokens.windows(5).any(|t| {
        t[0].text.eq_ignore_ascii_case("search")
            && t[1].text == ":"
            && t[2].text == ":"
            && t[3].text.eq_ignore_ascii_case("nearest")
            && t[4].text == "("
    });
    let word = |text: &str| tokens.iter().any(|t| t.text.eq_ignore_ascii_case(text));
    let distance = word("ST_DistanceSphere")
        || tokens.windows(4).any(|t| {
            t[0].text.eq_ignore_ascii_case("geo")
                && t[1].text == ":"
                && t[3].text.eq_ignore_ascii_case("distance")
        });
    Ok(source || (distance && word("ORDER") && word("LIMIT")))
}

/// One indexed range scan per key range. Ranges are disjoint, so each entry
/// appears at most once and UNION ALL needs no deduplication.
fn ranged_sql(
    index: &crate::Index,
    ranges: &[(i64, i64)],
    columns: &str,
    filter: &str,
    empty: &str,
) -> String {
    if ranges.is_empty() {
        return format!("SELECT {empty} WHERE 0");
    }
    morton::coalesce(ranges.to_vec(), STATEMENT_ARMS)
        .iter()
        .map(|(first, last)| {
            format!(
                "SELECT {columns} FROM {} INDEXED BY {} WHERE key >= {first} AND key <= {last} AND {filter}",
                crate::quote(&index.storage),
                crate::quote(&index.name)
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

pub(crate) fn distance_coordinates(args: &[Value]) -> Result<Value> {
    let [ax, ay, bx, by] = args else {
        return Err(invalid("distance coordinate arity"));
    };
    Ok(Value::Number(distance(
        coordinates(ax, ay)?,
        coordinates(bx, by)?,
    )))
}

fn latitude_bounds(latitude: f64, radius: f64) -> (f64, f64) {
    // The inverse haversine is ill-conditioned near antipodes. Widen in
    // haversine space, not just degrees, to cover roundoff in the predicate.
    let angle = (radius / EARTH_RADIUS_M).min(std::f64::consts::PI);
    let h = ((angle / 2.0).sin().powi(2) + 64.0 * f64::EPSILON).min(1.0);
    let delta = 2.0 * h.sqrt().asin().to_degrees();
    (
        (latitude - delta - 1e-9).max(-90.0),
        (latitude + delta + 1e-9).min(90.0),
    )
}

/// Longitude/latitude windows containing every point whose computed distance
/// from `center` is at most `radius`. The angle is widened in haversine space
/// as in `latitude_bounds`, and the longitude half-width uses the exact maximum
/// of a spherical cap, `asin(sin r / cos latitude)`, rounded outward.
fn radius_windows(center: (f64, f64), radius: f64) -> Vec<morton::Window> {
    let (south, north) = latitude_bounds(center.1, radius);
    let angle = (radius / EARTH_RADIUS_M).min(std::f64::consts::PI);
    let h = ((angle / 2.0).sin().powi(2) + 64.0 * f64::EPSILON).min(1.0);
    let widened = 2.0 * h.sqrt().asin();
    let spread = widened.sin() / center.1.to_radians().cos() * (1.0 + 1e-12);
    let longitudes = if south <= -90.0
        || north >= 90.0
        || widened >= std::f64::consts::FRAC_PI_2
        || spread >= 1.0
    {
        vec![(-180.0, 180.0)]
    } else {
        let delta = spread.asin().to_degrees() + 1e-9;
        longitude_intervals(center.0 - delta, center.0 + delta)
    };
    windows(&longitudes, south, north)
}

/// Windows of an inclusive viewport. `west > east` crosses the antimeridian.
fn bbox_windows(west: f64, south: f64, east: f64, north: f64) -> Vec<morton::Window> {
    let longitudes = if west <= east {
        vec![(west, east)]
    } else {
        vec![(west, 180.0), (-180.0, east)]
    };
    windows(&longitudes, south, north)
}

fn longitude_intervals(west: f64, east: f64) -> Vec<(f64, f64)> {
    if east - west >= 360.0 {
        vec![(-180.0, 180.0)]
    } else if west < -180.0 {
        vec![(west + 360.0, 180.0), (-180.0, east)]
    } else if east > 180.0 {
        vec![(west, 180.0), (-180.0, east - 360.0)]
    } else {
        vec![(west, east)]
    }
}

/// Points stored at longitude -180 and 180 are the same place, so an interval
/// reaching one seam also covers the other seam's column.
fn windows(longitudes: &[(f64, f64)], south: f64, north: f64) -> Vec<morton::Window> {
    let mut windows = longitudes
        .iter()
        .map(|(west, east)| morton::Window::from_degrees(*west, south, *east, north))
        .collect::<Vec<_>>();
    if longitudes.iter().any(|(_, east)| *east >= 180.0) {
        windows.push(morton::Window::from_degrees(-180.0, south, -180.0, north));
    }
    if longitudes.iter().any(|(west, _)| *west <= -180.0) {
        windows.push(morton::Window::from_degrees(180.0, south, 180.0, north));
    }
    windows
}

/// Inclusive viewport membership, treating longitudes -180 and 180 as equal.
pub(crate) fn in_bbox(p: (f64, f64), west: f64, south: f64, east: f64, north: f64) -> bool {
    let longitude = |x: f64| {
        if west <= east {
            west <= x && x <= east
        } else {
            x >= west || x <= east
        }
    };
    south <= p.1 && p.1 <= north && (longitude(p.0) || (p.0.abs() == 180.0 && longitude(-p.0)))
}

#[cfg(test)]
mod index_tests {
    use crate::{Database, Parameters, Value};
    fn setup() -> (Database, crate::Connection) {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        for sql in [
            "INSERT INTO places {id:places:a,location:geo::point(1,2)}",
            "CREATE SEARCH INDEX locations ON places(location) USING SPATIAL",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
        (db, c)
    }
    #[test]
    fn spatial_metadata_is_versioned_and_detects_incompatible_schema() {
        let (db, c) = setup();
        let collection = c.catalog("places").unwrap();
        assert_eq!(collection.version, super::CATALOG_VERSION);
        assert_eq!(
            collection.indexes[0].spatial,
            Some(super::Config::current())
        );
        let mut metadata = serde_json::to_value(&collection).unwrap();
        metadata["version"] = serde_json::json!(6);
        assert!(crate::catalog::decode(&metadata.to_string(), "places").is_err());
        metadata["version"] = serde_json::json!(7);
        metadata["indexes"][0]["spatial"]["format"] = serde_json::json!(3);
        assert!(crate::catalog::decode(&metadata.to_string(), "places").is_err());
        metadata["indexes"][0]["spatial"]["format"] = serde_json::json!(2);
        metadata["indexes"][0]["unique"] = serde_json::json!(true);
        assert!(crate::catalog::decode(&metadata.to_string(), "places").is_err());
        metadata["version"] = serde_json::json!(8);
        metadata["indexes"][0]["unique"] = serde_json::json!(false);
        assert!(crate::catalog::decode(&metadata.to_string(), "places").is_err());
        c.run("DROP INDEX locations", &[]).unwrap();
        assert!(db.connect().is_err());
    }

    /// Rewrite the index as 2.4.0 stored it: latitude-ordered entries and
    /// catalog version 3.
    fn downgrade_to_latitude_format(c: &crate::Connection) {
        downgrade_named(c, "places");
    }

    pub(super) fn downgrade_named(c: &crate::Connection, table: &str) {
        let mut collection = c.catalog(table).unwrap();
        let documents = c.documents(&collection).unwrap();
        let index = &mut collection.indexes[0];
        c.drop_index_storage(index).unwrap();
        index.spatial = None;
        c.run(&index.table_ddl(), &[]).unwrap();
        c.run(&index.index_ddl(), &[]).unwrap();
        for document in &documents {
            c.insert_index(index, document).unwrap();
        }
        collection.version = 3;
        c.run(
            "UPDATE __romedb_catalog SET metadata=?1 WHERE name=?2",
            &[
                crate::text(&serde_json::to_string(&collection).unwrap()),
                crate::text(table),
            ],
        )
        .unwrap();
        c.validate_storage_schema().unwrap();
    }

    #[test]
    fn latitude_format_indexes_keep_working_and_reindex_upgrades_them() {
        let db = Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE places", &Parameters::new())
            .unwrap();
        for i in 0..400u64 {
            let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
            let latitude = ((i * 104_729) % 18001) as f64 / 100.0 - 90.0;
            c.execute(
                &format!("INSERT INTO places {{id:type::record('places',{i}),location:geo::point({longitude},{latitude})}}"),
                &Parameters::new(),
            )
            .unwrap();
        }
        c.execute(
            "CREATE SEARCH INDEX locations ON places(location) USING SPATIAL",
            &Parameters::new(),
        )
        .unwrap();
        let queries = [
            "SELECT id, distance_m FROM search::near('locations', geo::point(10, 10), 2000000) ORDER BY distance_m, id",
            "SELECT id FROM search::bbox('locations', 170, -40, -170, 40) ORDER BY id",
            "SELECT id FROM search::polygon('locations', geo::envelope(-20, -30, 40, 10), 'covers') ORDER BY id",
            "SELECT id, distance_m FROM search::nearest('locations', geo::point(-170, 80), 25) ORDER BY distance_m, id",
        ];
        let run = |c: &crate::Connection| {
            queries
                .iter()
                .map(|sql| c.execute(sql, &Parameters::new()).unwrap().rows)
                .collect::<Vec<_>>()
        };
        let current = run(&c);
        assert!(current.iter().all(|rows| !rows.is_empty()));
        downgrade_to_latitude_format(&c);
        assert_eq!(c.catalog("places").unwrap().version, 3);
        assert_eq!(c.catalog("places").unwrap().indexes[0].spatial, None);
        assert_eq!(run(&c), current);
        c.check_collection_integrity("places", Default::default())
            .unwrap();
        c.execute(
            "CREATE SEARCH INDEX IF NOT EXISTS locations ON places(location) USING SPATIAL",
            &Parameters::new(),
        )
        .unwrap();
        assert_eq!(c.catalog("places").unwrap().indexes[0].spatial, None);
        c.reindex("locations").unwrap();
        let upgraded = c.catalog("places").unwrap();
        assert_eq!(upgraded.version, super::CATALOG_VERSION);
        assert_eq!(upgraded.indexes[0].spatial, Some(super::Config::current()));
        assert_eq!(run(&c), current);
        c.check_collection_integrity("places", Default::default())
            .unwrap();
    }
    #[test]
    fn interrupted_reindex_keeps_the_latitude_format_index() {
        let path = std::env::temp_dir().join(format!(
            "romedb-spatial-reindex-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = path.to_str().unwrap().to_owned();
        let none = Parameters::new();
        let near = "SELECT id, distance_m FROM search::near('locations', geo::point(10, 10), 3000000) ORDER BY distance_m, id";
        let expected = {
            let db = Database::open(&path).unwrap();
            let c = db.connect().unwrap();
            c.execute("CREATE TABLE places", &none).unwrap();
            c.execute("BEGIN", &none).unwrap();
            for i in 0..2000u64 {
                let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
                let latitude = ((i * 104_729) % 18001) as f64 / 100.0 - 90.0;
                c.execute(
                    &format!("INSERT INTO places {{id:type::record('places',{i}),location:geo::point({longitude},{latitude})}}"),
                    &none,
                )
                .unwrap();
            }
            c.execute(
                "CREATE SEARCH INDEX locations ON places(location) USING SPATIAL",
                &none,
            )
            .unwrap();
            c.execute("COMMIT", &none).unwrap();
            downgrade_to_latitude_format(&c);
            let expected = c.execute(near, &none).unwrap().rows;
            assert!(!expected.is_empty());

            // Cancelled before it starts, or by a deadline while rebuilding.
            let cancelled = crate::CancellationToken::new();
            cancelled.cancel();
            let error = c
                .execute_cancellable("REINDEX locations", &none, &cancelled)
                .unwrap_err();
            assert_eq!(error.code(), "FDB_CANCELLED");
            let deadline = crate::CancellationToken::with_deadline(
                std::time::Instant::now() + std::time::Duration::from_millis(2),
            );
            if let Err(error) = c.execute_cancellable("REINDEX locations", &none, &deadline) {
                assert_eq!(error.code(), "FDB_CANCELLED");
                assert_eq!(c.catalog("places").unwrap().indexes[0].spatial, None);
            } else {
                downgrade_to_latitude_format(&c);
            }

            // Rolled back after the rebuild finished.
            c.execute("BEGIN", &none).unwrap();
            c.execute("REINDEX locations", &none).unwrap();
            assert!(c.catalog("places").unwrap().indexes[0].spatial.is_some());
            c.execute("ROLLBACK", &none).unwrap();
            assert_eq!(c.catalog("places").unwrap().indexes[0].spatial, None);
            assert_eq!(c.execute(near, &none).unwrap().rows, expected);

            // Closed with the rebuild uncommitted.
            c.execute("BEGIN", &none).unwrap();
            c.execute("REINDEX locations", &none).unwrap();
            expected
        };
        let db = Database::open(&path).unwrap();
        let c = db.connect().unwrap();
        let collection = c.catalog("places").unwrap();
        assert_eq!(
            (collection.version, collection.indexes[0].spatial.clone()),
            (3, None)
        );
        assert_eq!(c.execute(near, &none).unwrap().rows, expected);
        c.check_collection_integrity("places", Default::default())
            .unwrap();
        c.execute("REINDEX locations", &none).unwrap();
        assert_eq!(
            c.catalog("places").unwrap().indexes[0].spatial,
            Some(super::Config::current())
        );
        assert_eq!(c.execute(near, &none).unwrap().rows, expected);
        drop(c);
        drop(db);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }
    #[test]
    fn spatial_audit_detects_stale_longitude_and_missing_entries() {
        for change in ["UPDATE {storage} SET longitude=3", "DELETE FROM {storage}"] {
            let (_db, c) = setup();
            let index = c.catalog("places").unwrap().indexes.remove(0);
            c.run(
                &change.replace("{storage}", &crate::quote(&index.storage)),
                &[],
            )
            .unwrap();
            assert!(c
                .check_collection_integrity("places", Default::default())
                .is_err());
        }
    }
    #[test]
    fn ordinary_indexes_keep_v1_catalog_encoding_and_reject_spatial_lookup() {
        let (_db, c) = setup();
        c.execute("INSERT INTO docs {n:1}", &Parameters::new())
            .unwrap();
        c.create_index("docs", "n", vec!["n".into()], false)
            .unwrap();
        assert_eq!(c.catalog("docs").unwrap().version, 2);
        let metadata = serde_json::to_value(c.catalog("docs").unwrap()).unwrap();
        assert!(metadata["indexes"][0].get("kind").is_none());
        assert!(c
            .lookup_index("places", "locations", &Value::Integer(1))
            .is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_bounds_cover_distance_roundoff_near_antipodal_poles() {
        let a = (0.0, 89.99999479428419);
        let b = (0.0, -89.9999964108325);
        let radius = distance(a, b);
        let (low, high) = latitude_bounds(a.1, radius);
        assert!(b.1 >= low && b.1 <= high);
    }

    #[test]
    fn nearby_longitudes_do_not_disappear_during_wrapping() {
        let a = (0.0, 0.0);
        let b = (1e-14, 0.0);
        assert!(distance(a, b) > 0.0);
        assert_eq!(distance(a, b), distance(b, a));
    }

    #[test]
    fn rejects_malformed_points_and_nonfinite_values() {
        let origin = call("geo_point", &[Value::Integer(0), Value::Integer(0)]).unwrap();
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(call("geo_point", &[Value::Number(bad), Value::Integer(0)]).is_err());
            assert!(call(
                "within",
                &[origin.clone(), origin.clone(), Value::Number(bad)]
            )
            .is_err());
        }
        let Value::Object(fields) = &origin else {
            unreachable!()
        };
        let mut altitude = fields.clone();
        altitude.insert(
            "coordinates".into(),
            Value::Array(vec![Value::Integer(0); 3]),
        );
        let mut extra = fields.clone();
        extra.insert("extra".into(), Value::Null);
        let mut wrong_type = fields.clone();
        wrong_type.insert("type".into(), Value::String("point".into()));
        let mut out_of_range = fields.clone();
        out_of_range.insert(
            "coordinates".into(),
            Value::Array(vec![Value::Integer(181), Value::Integer(0)]),
        );
        for bad in [altitude, extra, wrong_type, out_of_range] {
            assert!(call("distance", &[origin.clone(), Value::Object(bad)]).is_err());
        }
    }
}
