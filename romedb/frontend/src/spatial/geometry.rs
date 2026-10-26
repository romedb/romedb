//! Point, Polygon and MultiPolygon values, strict GeoJSON exchange and
//! point/region location under the straight longitude/latitude edge model.
use super::exact::{orientation, Position};
use crate::{Document, Error, Result, Value};
use std::cmp::Ordering;

pub(crate) const MAX_GEOJSON_BYTES: usize = 1 << 20;
pub(crate) const MAX_POSITIONS: usize = 100_000;
pub(crate) const MAX_RINGS: usize = 10_000;
pub(crate) const MAX_COMPONENTS: usize = 1_000;
pub(crate) const MAX_GEOJSON_DEPTH: usize = 32;
pub(crate) const MAX_TOPOLOGY_WORK: u64 = 20_000_000;

pub(crate) enum Geometry {
    Point(Position),
    Region(Region),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Location {
    Outside,
    Boundary,
    Inside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Polygon,
    MultiPolygon,
}

pub(crate) struct Region {
    kind: Kind,
    polygons: Vec<Vec<Vec<Position>>>,
    bounds: Bounds,
    shape: Shape,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    pub(crate) west: f64,
    pub(crate) south: f64,
    pub(crate) east: f64,
    pub(crate) north: f64,
}

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Validation(format!("geo: {message}"))
}

fn limit(message: impl std::fmt::Display) -> Error {
    Error::Limit(format!("geo: {message}"))
}

impl Geometry {
    pub(crate) fn from_value(value: &Value) -> Result<Self> {
        let Value::Object(fields) = value else {
            return Err(invalid("expected a geometry object"));
        };
        let Some(Value::String(kind)) = fields.get("type") else {
            return Err(invalid("expected a geometry type"));
        };
        if fields.len() != 2 || !fields.contains_key("coordinates") {
            return Err(invalid("expected only geometry type and coordinates"));
        }
        Self::from_coordinates(kind, &fields["coordinates"])
    }

    pub(crate) fn from_geojson(text: &str) -> Result<Self> {
        if text.len() > MAX_GEOJSON_BYTES {
            return Err(limit(format!(
                "GeoJSON input exceeds {MAX_GEOJSON_BYTES} bytes"
            )));
        }
        let Json::Object(members) = Json::parse(text)? else {
            return Err(invalid("GeoJSON geometry must be an object"));
        };
        let member = |name: &str| members.iter().find(|(key, _)| key == name).map(|(_, v)| v);
        let Some(Json::String(kind)) = member("type") else {
            return Err(invalid("GeoJSON geometry requires a string type member"));
        };
        if matches!(
            kind.as_str(),
            "Feature" | "FeatureCollection" | "GeometryCollection"
        ) {
            return Err(invalid(format!(
                "{kind} is not supported; pass a Point, Polygon or MultiPolygon geometry"
            )));
        }
        if member("crs").is_some() {
            return Err(invalid(
                "the GeoJSON crs member is not supported; coordinates are always WGS84",
            ));
        }
        let Some(coordinates) = member("coordinates") else {
            return Err(invalid("GeoJSON geometry requires a coordinates member"));
        };
        Self::from_coordinates(kind, &coordinates.to_value())
    }

    fn from_coordinates(kind: &str, coordinates: &Value) -> Result<Self> {
        let mut budget = MAX_POSITIONS;
        match kind {
            "Point" => Ok(Self::Point(position(coordinates)?)),
            "Polygon" => {
                let polygon = polygon(coordinates, &mut budget)?;
                Ok(Self::Region(Region::new(Kind::Polygon, vec![polygon])?))
            }
            "MultiPolygon" => {
                let Value::Array(items) = coordinates else {
                    return Err(invalid(
                        "MultiPolygon coordinates must be an array of polygons",
                    ));
                };
                if items.is_empty() {
                    return Err(invalid("MultiPolygon requires at least one polygon"));
                }
                if items.len() > MAX_COMPONENTS {
                    return Err(limit(format!(
                        "MultiPolygon exceeds {MAX_COMPONENTS} polygons"
                    )));
                }
                let polygons = items
                    .iter()
                    .map(|item| polygon(item, &mut budget))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Self::Region(Region::new(Kind::MultiPolygon, polygons)?))
            }
            "LineString" | "MultiLineString" | "MultiPoint" | "GeometryCollection" => {
                Err(invalid(format!("{kind} geometry is not supported")))
            }
            _ => Err(invalid(format!("unknown geometry type {kind}"))),
        }
    }

    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Point(p) => object("Point", position_value(*p)),
            Self::Region(region) => region.to_value(),
        }
    }

    pub(crate) fn to_geojson(&self) -> Result<String> {
        let (kind, coordinates) = match self {
            Self::Point(p) => ("Point", position_json(*p)?),
            Self::Region(region) => {
                let polygons = region
                    .polygons
                    .iter()
                    .map(|rings| polygon_json(rings))
                    .collect::<Result<Vec<_>>>()?;
                match region.kind {
                    Kind::Polygon => ("Polygon", polygons.into_iter().next().expect("one polygon")),
                    Kind::MultiPolygon => ("MultiPolygon", serde_json::Value::Array(polygons)),
                }
            }
        };
        Ok(format!(
            "{{\"type\":\"{kind}\",\"coordinates\":{}}}",
            serde_json::to_string(&coordinates)?
        ))
    }
}

pub(crate) fn position(value: &Value) -> Result<Position> {
    let Value::Array(values) = value else {
        return Err(invalid("expected a coordinate array"));
    };
    let [longitude, latitude] = values.as_slice() else {
        return Err(invalid("expected two coordinates"));
    };
    super::coordinates(longitude, latitude)
}

fn polygon(value: &Value, budget: &mut usize) -> Result<Vec<Vec<Position>>> {
    let Value::Array(rings) = value else {
        return Err(invalid("Polygon coordinates must be an array of rings"));
    };
    if rings.is_empty() {
        return Err(invalid("Polygon requires an exterior ring"));
    }
    rings
        .iter()
        .map(|ring| {
            let Value::Array(positions) = ring else {
                return Err(invalid("a ring must be an array of positions"));
            };
            if positions.len() > *budget {
                return Err(limit(format!("geometry exceeds {MAX_POSITIONS} positions")));
            }
            *budget -= positions.len();
            positions.iter().map(position).collect()
        })
        .collect()
}

fn object(kind: &str, coordinates: Value) -> Value {
    Value::Object(Document::from([
        ("type".into(), Value::String(kind.into())),
        ("coordinates".into(), coordinates),
    ]))
}

fn position_value(p: Position) -> Value {
    Value::Array(vec![Value::Number(p.0), Value::Number(p.1)])
}

fn position_json(p: Position) -> Result<serde_json::Value> {
    let number = |n: f64| {
        serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .ok_or_else(|| invalid("coordinates must be finite"))
    };
    Ok(serde_json::Value::Array(vec![number(p.0)?, number(p.1)?]))
}

fn polygon_json(rings: &[Vec<Position>]) -> Result<serde_json::Value> {
    Ok(serde_json::Value::Array(
        rings
            .iter()
            .map(|ring| {
                Ok(serde_json::Value::Array(
                    ring.iter()
                        .map(|p| position_json(*p))
                        .collect::<Result<_>>()?,
                ))
            })
            .collect::<Result<_>>()?,
    ))
}

impl Region {
    pub(crate) fn new(kind: Kind, polygons: Vec<Vec<Vec<Position>>>) -> Result<Self> {
        let ring_count: usize = polygons.iter().map(Vec::len).sum();
        if ring_count > MAX_RINGS {
            return Err(limit(format!("geometry exceeds {MAX_RINGS} rings")));
        }
        for (p, rings) in polygons.iter().enumerate() {
            for (r, ring) in rings.iter().enumerate() {
                validate_ring(ring).map_err(|e| ring_error(kind, p, r, e))?;
            }
        }
        let mut work = 0u64;
        validate_topology(kind, &polygons, &mut work)?;
        let shape = Shape::new(polygons.iter().flatten().map(Vec::as_slice));
        let region = Self {
            kind,
            bounds: shape.bounds,
            polygons,
            shape,
        };
        region.validate_nesting(&mut work)?;
        Ok(region)
    }

    pub(crate) fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub(crate) fn edges(&self) -> &[(Position, Position)] {
        &self.shape.edges
    }

    /// Classify a point, treating longitudes -180 and 180 as the same place.
    pub(crate) fn locate(&self, point: Position) -> Location {
        let mut best = self.locate_exact(point);
        if point.0.abs() == 180.0 && best != Location::Inside {
            best = best.max(self.locate_exact((-point.0, point.1)));
        }
        best
    }

    fn locate_exact(&self, point: Position) -> Location {
        if !self.bounds.contains(point) {
            return Location::Outside;
        }
        self.shape.locate(point, &mut 0)
    }

    pub(crate) fn to_value(&self) -> Value {
        let polygon_value = |rings: &Vec<Vec<Position>>| {
            Value::Array(
                rings
                    .iter()
                    .map(|ring| Value::Array(ring.iter().map(|p| position_value(*p)).collect()))
                    .collect(),
            )
        };
        match self.kind {
            Kind::Polygon => object("Polygon", polygon_value(&self.polygons[0])),
            Kind::MultiPolygon => object(
                "MultiPolygon",
                Value::Array(self.polygons.iter().map(polygon_value).collect()),
            ),
        }
    }

    fn validate_nesting(&self, work: &mut u64) -> Result<()> {
        for (p, rings) in self.polygons.iter().enumerate() {
            let exterior = Shape::new(std::iter::once(rings[0].as_slice()));
            let holes = rings[1..]
                .iter()
                .map(|ring| Shape::new(std::iter::once(ring.as_slice())))
                .collect::<Vec<_>>();
            for (h, hole) in rings[1..].iter().enumerate() {
                if exterior.locate(hole[0], work) != Location::Inside {
                    return Err(ring_error(
                        self.kind,
                        p,
                        h + 1,
                        invalid("hole lies outside the exterior ring"),
                    ));
                }
                for (other, shape) in holes.iter().enumerate() {
                    charge(work, 0)?;
                    if other != h
                        && shape.bounds.overlaps(&holes[h].bounds)
                        && shape.locate(hole[0], work) == Location::Inside
                    {
                        return Err(ring_error(
                            self.kind,
                            p,
                            h + 1,
                            invalid("holes must not overlap"),
                        ));
                    }
                }
            }
        }
        if self.polygons.len() < 2 {
            return Ok(());
        }
        let components = self
            .polygons
            .iter()
            .map(|rings| Shape::new(rings.iter().map(Vec::as_slice)))
            .collect::<Vec<_>>();
        for (a, rings) in self.polygons.iter().enumerate() {
            for (b, other) in components.iter().enumerate() {
                charge(work, 0)?;
                if a == b || !components[a].bounds.overlaps(&other.bounds) {
                    continue;
                }
                for ring in rings {
                    for edge in ring.windows(2) {
                        let middle = ((edge[0].0 + edge[1].0) / 2.0, (edge[0].1 + edge[1].1) / 2.0);
                        for probe in [edge[0], middle] {
                            if other.locate(probe, work) == Location::Inside {
                                return Err(invalid(format!(
                                    "MultiPolygon polygons {a} and {b} overlap"
                                )));
                            }
                        }
                        charge(work, 0)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn ring_error(kind: Kind, polygon: usize, ring: usize, error: Error) -> Error {
    let Error::Validation(message) = error else {
        return error;
    };
    let message = message.trim_start_matches("geo: ");
    let place = match kind {
        Kind::Polygon => format!("ring {ring}"),
        Kind::MultiPolygon => format!("polygon {polygon} ring {ring}"),
    };
    invalid(format!("{place}: {message}"))
}

fn charge(work: &mut u64, amount: u64) -> Result<()> {
    *work += amount + 1;
    if *work > MAX_TOPOLOGY_WORK {
        return Err(limit("geometry validation exceeds its work limit"));
    }
    Ok(())
}

fn validate_ring(ring: &[Position]) -> Result<()> {
    if ring.len() < 4 {
        return Err(invalid("a ring requires at least four positions"));
    }
    if ring[0] != ring[ring.len() - 1] {
        return Err(invalid("a ring must end at its first position"));
    }
    for (i, pair) in ring.windows(2).enumerate() {
        if pair[0] == pair[1] {
            return Err(invalid(format!(
                "position {} repeats the previous position",
                i + 1
            )));
        }
        if (pair[1].0 - pair[0].0).abs() >= 180.0 {
            return Err(invalid(format!(
                "edge {i} spans 180 or more degrees of longitude; split the polygon at the antimeridian"
            )));
        }
    }
    if let Some(i) = ring.iter().position(|p| p.1.abs() == 90.0) {
        return Err(invalid(format!(
            "position {i} touches a pole; polar polygons are not supported"
        )));
    }
    let first = ring[0];
    let Some(second) = ring.iter().copied().find(|p| *p != first) else {
        return Err(invalid("a ring requires three distinct positions"));
    };
    if !ring
        .iter()
        .any(|p| orientation(first, second, *p) != Ordering::Equal)
    {
        return Err(invalid("a ring must not be collinear"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Contact {
    Disjoint,
    Touch,
    Cross,
    Overlap,
}

struct Segment {
    a: Position,
    b: Position,
    polygon: usize,
    ring: usize,
    index: usize,
    edges: usize,
    west: f64,
    east: f64,
    south: f64,
    north: f64,
}

fn validate_topology(kind: Kind, polygons: &[Vec<Vec<Position>>], work: &mut u64) -> Result<()> {
    let mut segments = Vec::new();
    for (polygon, rings) in polygons.iter().enumerate() {
        for (ring, positions) in rings.iter().enumerate() {
            let edges = positions.len() - 1;
            for (index, pair) in positions.windows(2).enumerate() {
                let (a, b) = (pair[0], pair[1]);
                segments.push(Segment {
                    a,
                    b,
                    polygon,
                    ring,
                    index,
                    edges,
                    west: a.0.min(b.0),
                    east: a.0.max(b.0),
                    south: a.1.min(b.1),
                    north: a.1.max(b.1),
                });
            }
        }
    }
    segments.sort_by(|s, t| s.west.total_cmp(&t.west));
    for (i, s) in segments.iter().enumerate() {
        for t in &segments[i + 1..] {
            if t.west > s.east {
                break;
            }
            charge(work, 0)?;
            if t.north < s.south || t.south > s.north {
                continue;
            }
            check_pair(kind, s, t)?;
        }
    }
    Ok(())
}

fn check_pair(kind: Kind, s: &Segment, t: &Segment) -> Result<()> {
    let contact = contact(s.a, s.b, t.a, t.b);
    if contact == Contact::Disjoint {
        return Ok(());
    }
    if s.polygon != t.polygon {
        if matches!(contact, Contact::Cross | Contact::Overlap) {
            return Err(invalid(format!(
                "MultiPolygon polygons {} and {} overlap or share an edge",
                s.polygon.min(t.polygon),
                s.polygon.max(t.polygon)
            )));
        }
        return Ok(());
    }
    if s.ring != t.ring {
        return Err(ring_error(
            kind,
            s.polygon,
            s.ring.max(t.ring),
            invalid("rings of a polygon must not touch or cross"),
        ));
    }
    let adjacent = (s.index + 1) % s.edges == t.index || (t.index + 1) % s.edges == s.index;
    if adjacent && contact != Contact::Overlap {
        return Ok(());
    }
    Err(ring_error(
        kind,
        s.polygon,
        s.ring,
        invalid(format!(
            "edges {} and {} intersect; rings must not cross or touch themselves",
            s.index.min(t.index),
            s.index.max(t.index)
        )),
    ))
}

fn contact(a: Position, b: Position, c: Position, d: Position) -> Contact {
    let o1 = orientation(a, b, c);
    let o2 = orientation(a, b, d);
    let o3 = orientation(c, d, a);
    let o4 = orientation(c, d, b);
    if [o1, o2, o3, o4].iter().all(|o| *o == Ordering::Equal) {
        let (s0, s1) = ordered(a, b);
        let (t0, t1) = ordered(c, d);
        let low = if lexical(s0, t0) == Ordering::Less {
            t0
        } else {
            s0
        };
        let high = if lexical(s1, t1) == Ordering::Less {
            s1
        } else {
            t1
        };
        return match lexical(low, high) {
            Ordering::Less => Contact::Overlap,
            Ordering::Equal => Contact::Touch,
            Ordering::Greater => Contact::Disjoint,
        };
    }
    let separated = |x: Ordering, y: Ordering| x == y && x != Ordering::Equal;
    if separated(o1, o2) || separated(o3, o4) {
        return Contact::Disjoint;
    }
    if [o1, o2, o3, o4].contains(&Ordering::Equal) {
        Contact::Touch
    } else {
        Contact::Cross
    }
}

fn ordered(a: Position, b: Position) -> (Position, Position) {
    if lexical(a, b) == Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    }
}

fn lexical(a: Position, b: Position) -> Ordering {
    a.0.partial_cmp(&b.0)
        .expect("finite")
        .then_with(|| a.1.partial_cmp(&b.1).expect("finite"))
}

impl Bounds {
    fn contains(&self, p: Position) -> bool {
        self.west <= p.0 && p.0 <= self.east && self.south <= p.1 && p.1 <= self.north
    }
    fn overlaps(&self, other: &Self) -> bool {
        self.west <= other.east
            && other.west <= self.east
            && self.south <= other.north
            && other.south <= self.north
    }
}

/// Edges of one or more rings, bucketed into latitude strips. A point's
/// boundary test and its eastward crossing count only need edges whose
/// latitude range includes the point, so only its own strip is scanned.
struct Shape {
    edges: Vec<(Position, Position)>,
    bounds: Bounds,
    low: f64,
    scale: f64,
    strips: usize,
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl Shape {
    fn new<'a>(rings: impl Iterator<Item = &'a [Position]>) -> Self {
        let edges = rings
            .flat_map(|ring| ring.windows(2).map(|pair| (pair[0], pair[1])))
            .collect::<Vec<_>>();
        let mut bounds = Bounds {
            west: f64::INFINITY,
            south: f64::INFINITY,
            east: f64::NEG_INFINITY,
            north: f64::NEG_INFINITY,
        };
        for (a, _) in &edges {
            bounds.west = bounds.west.min(a.0);
            bounds.east = bounds.east.max(a.0);
            bounds.south = bounds.south.min(a.1);
            bounds.north = bounds.north.max(a.1);
        }
        let mut strips = (edges.len() / 4).clamp(1, 4096);
        loop {
            let shape = Self::bucket(edges.clone(), bounds, strips);
            if shape.items.len() <= 8 * shape.edges.len() + strips || strips == 1 {
                return shape;
            }
            strips /= 2;
        }
    }

    fn bucket(edges: Vec<(Position, Position)>, bounds: Bounds, strips: usize) -> Self {
        let height = bounds.north - bounds.south;
        let scale = if height > 0.0 {
            strips as f64 / height
        } else {
            0.0
        };
        let mut shape = Self {
            edges,
            bounds,
            low: bounds.south,
            scale,
            strips,
            starts: vec![0; strips + 1],
            items: Vec::new(),
        };
        let spans = shape
            .edges
            .iter()
            .map(|(a, b)| (shape.strip(a.1.min(b.1)), shape.strip(a.1.max(b.1))))
            .collect::<Vec<_>>();
        for &(first, last) in &spans {
            for strip in first..=last {
                shape.starts[strip + 1] += 1;
            }
        }
        for strip in 0..strips {
            shape.starts[strip + 1] += shape.starts[strip];
        }
        let mut next = shape.starts.clone();
        shape.items = vec![0; shape.starts[strips] as usize];
        for (edge, &(first, last)) in spans.iter().enumerate() {
            for slot in &mut next[first..=last] {
                shape.items[*slot as usize] = edge as u32;
                *slot += 1;
            }
        }
        shape
    }

    fn strip(&self, latitude: f64) -> usize {
        let index = ((latitude - self.low) * self.scale).floor();
        if index <= 0.0 {
            0
        } else {
            (index as usize).min(self.strips - 1)
        }
    }

    fn locate(&self, p: Position, work: &mut u64) -> Location {
        let strip = self.strip(p.1);
        let items = &self.items[self.starts[strip] as usize..self.starts[strip + 1] as usize];
        *work += items.len() as u64;
        let mut inside = false;
        for &edge in items {
            let (a, b) = self.edges[edge as usize];
            let upward = a.1 <= p.1 && b.1 > p.1;
            let downward = a.1 > p.1 && b.1 <= p.1;
            let within = a.1.min(b.1) <= p.1
                && p.1 <= a.1.max(b.1)
                && a.0.min(b.0) <= p.0
                && p.0 <= a.0.max(b.0);
            if !upward && !downward && !within {
                continue;
            }
            let side = orientation(a, b, p);
            if within && side == Ordering::Equal {
                return Location::Boundary;
            }
            if (upward && side == Ordering::Greater) || (downward && side == Ordering::Less) {
                inside = !inside;
            }
        }
        if inside {
            Location::Inside
        } else {
            Location::Outside
        }
    }
}

/// JSON tree that rejects duplicate object keys.
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Result<Self> {
        let mut depth = 0usize;
        let mut quoted = false;
        let mut escaped = false;
        for byte in text.bytes() {
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
                continue;
            }
            match byte {
                b'"' => quoted = true,
                b'[' | b'{' => {
                    depth += 1;
                    if depth > MAX_GEOJSON_DEPTH {
                        return Err(limit(format!(
                            "GeoJSON nesting exceeds {MAX_GEOJSON_DEPTH}"
                        )));
                    }
                }
                b']' | b'}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        let mut decoder = serde_json::Deserializer::from_str(text);
        let json = serde::Deserialize::deserialize(&mut decoder)
            .and_then(|json| decoder.end().map(|()| json))
            .map_err(|e| invalid(format!("malformed GeoJSON: {e}")))?;
        Ok(json)
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Boolean(*b),
            Self::Number(n) => Value::Number(*n),
            Self::String(s) => Value::String(s.clone()),
            Self::Array(items) => Value::Array(items.iter().map(Self::to_value).collect()),
            Self::Object(members) => Value::Object(
                members
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }
}

impl<'de> serde::Deserialize<'de> for Json {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> serde::de::Visitor<'de> for JsonVisitor {
    type Value = Json;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E>(self) -> std::result::Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E>(self, v: bool) -> std::result::Result<Json, E> {
        Ok(Json::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> std::result::Result<Json, E> {
        Ok(Json::Number(v as f64))
    }
    fn visit_u64<E>(self, v: u64) -> std::result::Result<Json, E> {
        Ok(Json::Number(v as f64))
    }
    fn visit_f64<E>(self, v: f64) -> std::result::Result<Json, E> {
        Ok(Json::Number(v))
    }
    fn visit_str<E>(self, v: &str) -> std::result::Result<Json, E> {
        Ok(Json::String(v.into()))
    }
    fn visit_string<E>(self, v: String) -> std::result::Result<Json, E> {
        Ok(Json::String(v))
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Json, A::Error> {
        let mut members: Vec<(String, Json)> = Vec::new();
        let mut keys = std::collections::BTreeSet::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
            }
            members.push((key, value));
        }
        Ok(Json::Object(members))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(text: &str) -> Region {
        match Geometry::from_geojson(text).unwrap() {
            Geometry::Region(region) => region,
            Geometry::Point(_) => panic!("expected region"),
        }
    }

    fn error(text: &str) -> String {
        match Geometry::from_geojson(text) {
            Ok(_) => panic!("accepted {text}"),
            Err(e) => e.to_string(),
        }
    }

    const SQUARE_WITH_HOLE: &str = r#"{"type":"Polygon","coordinates":[
        [[0,0],[10,0],[10,10],[0,10],[0,0]],
        [[4,4],[4,6],[6,6],[6,4],[4,4]]]}"#;

    #[test]
    fn locates_interior_boundary_hole_and_exterior_points() {
        let r = region(SQUARE_WITH_HOLE);
        let cases = [
            ((1.0, 1.0), Location::Inside),
            ((0.0, 5.0), Location::Boundary),
            ((10.0, 10.0), Location::Boundary),
            ((5.0, 0.0), Location::Boundary),
            ((5.0, 5.0), Location::Outside),
            ((4.0, 5.0), Location::Boundary),
            ((6.0, 6.0), Location::Boundary),
            ((11.0, 5.0), Location::Outside),
            ((-1.0, 0.0), Location::Outside),
            ((3.0, 4.0), Location::Inside),
            ((7.0, 6.0), Location::Inside),
        ];
        for (p, expected) in cases {
            assert_eq!(r.locate(p), expected, "{p:?}");
        }
    }

    #[test]
    fn winding_direction_does_not_change_location() {
        let clockwise = region(
            r#"{"type":"Polygon","coordinates":[[[0,0],[0,10],[10,10],[10,0],[0,0]],[[4,4],[6,4],[6,6],[4,6],[4,4]]]}"#,
        );
        let counter = region(SQUARE_WITH_HOLE);
        for x in 0..=24 {
            for y in 0..=24 {
                let p = (x as f64 * 0.5 - 1.0, y as f64 * 0.5 - 1.0);
                assert_eq!(clockwise.locate(p), counter.locate(p), "{p:?}");
            }
        }
    }

    #[test]
    fn concave_and_horizontal_edges_use_half_open_crossings() {
        let r = region(
            r#"{"type":"Polygon","coordinates":[[[0,0],[6,0],[6,6],[4,6],[4,2],[2,2],[2,6],[0,6],[0,0]]]}"#,
        );
        assert_eq!(r.locate((3.0, 4.0)), Location::Outside);
        assert_eq!(r.locate((3.0, 2.0)), Location::Boundary);
        assert_eq!(r.locate((1.0, 2.0)), Location::Inside);
        assert_eq!(r.locate((5.0, 6.0)), Location::Boundary);
        assert_eq!(r.locate((-1.0, 6.0)), Location::Outside);
        assert_eq!(r.locate((-1.0, 2.0)), Location::Outside);
        assert_eq!(r.locate((7.0, 0.0)), Location::Outside);
    }

    #[test]
    fn rejects_invalid_rings_and_topology() {
        let cases = [
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[0,0]]]}"#,
                "four positions",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}"#,
                "first position",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,0],[1,1],[0,0]]]}"#,
                "repeats",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[2,0],[0,0]]]}"#,
                "collinear",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[2,2],[2,0],[0,2],[0,0]]]}"#,
                "intersect",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[2,0],[2,2],[0,0]]]}"#,
                "intersect",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[2,0],[0,4],[0,0]]]}"#,
                "intersect",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[5,5],[6,5],[6,6],[5,5]]]}"#,
                "outside the exterior",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[0,1],[1,1],[1,2],[0,1]]]}"#,
                "must not touch",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[9,0],[9,9],[0,9],[0,0]],[[1,1],[3,1],[3,3],[1,1]],[[3,3],[5,3],[5,5],[3,3]]]}"#,
                "must not touch",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[9,0],[9,9],[0,9],[0,0]],[[1,1],[8,1],[8,8],[1,8],[1,1]],[[2,2],[3,2],[3,3],[2,2]]]}"#,
                "overlap",
            ),
            (
                r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[2,0],[2,2],[0,2],[0,0]]],[[[1,1],[3,1],[3,3],[1,3],[1,1]]]]}"#,
                "overlap",
            ),
            (
                r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[2,0],[2,2],[0,2],[0,0]]],[[[2,0],[4,0],[4,2],[2,2],[2,0]]]]}"#,
                "share an edge",
            ),
            (
                r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[9,0],[9,9],[0,9],[0,0]]],[[[2,2],[3,2],[3,3],[2,2]]]]}"#,
                "overlap",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[170,0],[-170,0],[-170,1],[170,0]]]}"#,
                "antimeridian",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,80],[10,90],[20,80],[0,80]]]}"#,
                "pole",
            ),
            (r#"{"type":"Polygon","coordinates":[]}"#, "exterior ring"),
            (
                r#"{"type":"MultiPolygon","coordinates":[]}"#,
                "at least one",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1,5],[0,0]]]}"#,
                "two coordinates",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],[181,0],[1,1],[0,0]]]}"#,
                "longitude",
            ),
            (
                r#"{"type":"Polygon","coordinates":[[[0,0],["1",0],[1,1],[0,0]]]}"#,
                "finite number",
            ),
        ];
        for (text, expected) in cases {
            let message = error(text);
            assert!(message.contains(expected), "{text}: {message}");
        }
    }

    #[test]
    fn accepts_vertex_contact_islands_and_split_antimeridian_regions() {
        let touching = region(
            r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[2,0],[2,2],[0,2],[0,0]]],[[[2,2],[4,2],[4,4],[2,4],[2,2]]]]}"#,
        );
        assert_eq!(touching.locate((2.0, 2.0)), Location::Boundary);
        assert_eq!(touching.locate((3.0, 3.0)), Location::Inside);
        let island = region(
            r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[9,0],[9,9],[0,9],[0,0]],[[2,2],[7,2],[7,7],[2,7],[2,2]]],[[[3,3],[6,3],[6,6],[3,6],[3,3]]]]}"#,
        );
        assert_eq!(island.locate((4.0, 4.0)), Location::Inside);
        assert_eq!(island.locate((2.5, 2.5)), Location::Outside);
        assert_eq!(island.locate((1.0, 1.0)), Location::Inside);
        let split = region(
            r#"{"type":"MultiPolygon","coordinates":[[[[170,-10],[180,-10],[180,10],[170,10],[170,-10]]],[[[-180,-10],[-170,-10],[-170,10],[-180,10],[-180,-10]]]]}"#,
        );
        for p in [(180.0, 0.0), (-180.0, 0.0)] {
            assert_eq!(split.locate(p), Location::Boundary);
        }
        assert_eq!(split.locate((175.0, 0.0)), Location::Inside);
        assert_eq!(split.locate((-175.0, 0.0)), Location::Inside);
        assert_eq!(split.locate((0.0, 0.0)), Location::Outside);
    }

    #[test]
    fn geojson_rules_reject_wrappers_crs_and_duplicates_and_ignore_metadata() {
        assert!(error(r#"{"type":"Feature","geometry":null,"properties":{}}"#).contains("Feature"));
        assert!(error(r#"{"type":"Point","coordinates":[1,2],"crs":{}}"#).contains("crs"));
        assert!(
            error(r#"{"type":"Point","coordinates":[1,2],"type":"Point"}"#).contains("duplicate")
        );
        assert!(
            error(r#"{"type":"Point","coordinates":[1,2],"x":{"a":1,"a":2}}"#)
                .contains("duplicate")
        );
        assert!(
            error(r#"{"type":"LineString","coordinates":[[1,2],[3,4]]}"#).contains("not supported")
        );
        assert!(error(r#"{"type":"Point","coordinates":[1,2]"#).contains("malformed"));
        assert!(error(&format!("{}1{}", "[".repeat(40), "]".repeat(40))).contains("nesting"));
        let point = Geometry::from_geojson(
            r#"{"type":"Point","coordinates":[106.7,10.8],"bbox":[0,0,0,0],"id":7,"properties":{"a":1}}"#,
        )
        .unwrap();
        assert_eq!(
            point.to_geojson().unwrap(),
            r#"{"type":"Point","coordinates":[106.7,10.8]}"#
        );
    }

    #[test]
    fn geojson_round_trip_preserves_coordinates_and_order() {
        let text = r#"{"type":"MultiPolygon","coordinates":[[[[0.1,0.2],[0.30000000000000004,0.2],[0.3,5e-324],[-0.0,0.2],[0.1,0.2]]]]}"#;
        let geometry = Geometry::from_geojson(text).unwrap();
        let output = geometry.to_geojson().unwrap();
        let again = Geometry::from_geojson(&output).unwrap();
        assert_eq!(again.to_geojson().unwrap(), output);
        let value = geometry.to_value();
        let from_value = Geometry::from_value(&value).unwrap();
        assert_eq!(from_value.to_geojson().unwrap(), output);
        let Geometry::Region(region) = from_value else {
            panic!()
        };
        assert_eq!(region.polygons[0][0][1], (0.30000000000000004, 0.2));
        assert!(region.polygons[0][0][3].0.is_sign_negative());
    }

    fn outline(n: usize, spike: f64) -> Vec<Position> {
        let mut ring = (0..n)
            .map(|i| {
                let angle = i as f64 / n as f64 * std::f64::consts::TAU;
                let radius = if i % 2 == 0 { 10.0 } else { 10.0 - spike };
                (radius * angle.cos(), radius * angle.sin() / 2.0)
            })
            .collect::<Vec<_>>();
        ring.push(ring[0]);
        ring
    }

    #[test]
    #[ignore = "cost assessment; run with --profile romedb-production -- --ignored --nocapture"]
    fn geometry_cost_assessment() {
        use std::time::Instant;
        let shapes = [
            ("outline", 1_000, 0.01),
            ("outline", 10_000, 0.001),
            ("outline", 99_998, 0.0001),
            ("star", 1_000, 4.0),
            ("star", 10_000, 4.0),
            ("star", 99_998, 4.0),
        ];
        for (name, n, spike) in shapes {
            let start = Instant::now();
            let region = Region::new(Kind::Polygon, vec![vec![outline(n, spike)]]);
            let build = start.elapsed();
            let region = match region {
                Ok(region) => region,
                Err(e) => {
                    println!("{name} {n} vertices: {} after {build:?}", e.code());
                    continue;
                }
            };
            let start = Instant::now();
            let mut inside = 0;
            for i in 0..100_000 {
                let p = (
                    (i % 400) as f64 * 0.05 - 10.0,
                    (i / 400) as f64 * 0.04 - 5.0,
                );
                inside += (region.locate(p) == Location::Inside) as usize;
            }
            let per_point = start.elapsed() / 100_000;
            println!(
                "{name} {n} vertices: validate+prepare {build:?}, locate {per_point:?}/point, {} strips, {inside} inside",
                region.shape.strips
            );
        }
        let squares = (0..1_000)
            .map(|i| {
                let (x, y) = ((i % 40) as f64 * 2.0, (i / 40) as f64 * 2.0);
                vec![vec![
                    (x, y),
                    (x + 1.0, y),
                    (x + 1.0, y + 1.0),
                    (x, y + 1.0),
                    (x, y),
                ]]
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        let result = Region::new(Kind::MultiPolygon, squares)
            .err()
            .map(|e| e.code());
        println!(
            "MultiPolygon 1000 components: {result:?} after {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn strip_index_matches_full_scan_on_large_polygon() {
        let n = 2000;
        let mut ring = (0..n)
            .map(|i| {
                let angle = i as f64 / n as f64 * std::f64::consts::TAU;
                let radius = if i % 2 == 0 { 10.0 } else { 6.0 };
                (radius * angle.cos(), radius * angle.sin() / 2.0)
            })
            .collect::<Vec<_>>();
        ring.push(ring[0]);
        let r = Region::new(Kind::Polygon, vec![vec![ring.clone()]]).unwrap();
        assert!(r.shape.strips > 1);
        let full = Shape::bucket(r.shape.edges.clone(), r.bounds, 1);
        for x in -22..=22 {
            for y in -12..=12 {
                let p = (x as f64 * 0.5, y as f64 * 0.5);
                assert_eq!(r.locate(p), full.locate(p, &mut 0), "{p:?}");
            }
        }
        for p in &ring {
            assert_eq!(r.locate(*p), Location::Boundary);
        }
    }
}
