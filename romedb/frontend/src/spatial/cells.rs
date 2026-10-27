//! H3 cell hierarchy, neighborhood, distance, area and boundary functions.
use super::exact::Position;
use super::geometry::{Kind, Region};
use crate::{Error, Result, Value};
use h3o::CellIndex;

pub(crate) const MAX_CELLS: u64 = 10_000;

fn invalid(message: impl std::fmt::Display) -> Error {
    Error::Validation(format!("geo: {message}"))
}

fn too_many(count: u64) -> Error {
    Error::Limit(format!(
        "geo: cell result of {count} cells exceeds {MAX_CELLS}"
    ))
}

pub(super) fn call(operation: &str, args: &[Value]) -> Result<Value> {
    match (operation, args) {
        ("cell_boundary", [cell]) => boundary(parse(cell)?),
        ("cell_parent", [cell, resolution]) => {
            let cell = parse(cell)?;
            let resolution = super::resolution(resolution)?;
            let parent = cell.parent(resolution).ok_or_else(|| {
                invalid("parent resolution must not be finer than the cell resolution")
            })?;
            Ok(Value::String(parent.to_string()))
        }
        ("cell_children", [cell, resolution]) => {
            let cell = parse(cell)?;
            let resolution = super::resolution(resolution)?;
            if resolution < cell.resolution() {
                return Err(invalid(
                    "child resolution must not be coarser than the cell resolution",
                ));
            }
            let count = cell.children_count(resolution);
            if count > MAX_CELLS {
                return Err(too_many(count));
            }
            Ok(addresses(cell.children(resolution).collect()))
        }
        ("cell_disk", [cell, k]) => {
            let cell = parse(cell)?;
            let k = super::number(k)?;
            if k < 0.0 || k.fract() != 0.0 || k > u32::MAX as f64 {
                return Err(invalid("grid disk distance must be a nonnegative integer"));
            }
            let k = k as u64;
            let count = 3 * k * (k + 1) + 1;
            if count > MAX_CELLS {
                return Err(too_many(count));
            }
            Ok(addresses(cell.grid_disk::<Vec<_>>(k as u32)))
        }
        ("cell_resolution", [cell]) => {
            Ok(Value::Integer(u8::from(parse(cell)?.resolution()).into()))
        }
        ("cell_distance", [a, b]) => {
            let distance = parse(a)?
                .grid_distance(parse(b)?)
                .map_err(|_| invalid("grid distance is undefined for these cells"))?;
            Ok(Value::Integer(distance.into()))
        }
        ("cell_area", [cell]) => Ok(Value::Number(parse(cell)?.area_m2())),
        ("cell_compact", [cells]) => {
            let mut cells = list(cells)?;
            if let Some(first) = cells.first() {
                let resolution = first.resolution();
                if cells.iter().any(|cell| cell.resolution() != resolution) {
                    return Err(invalid("cells to compact must share one resolution"));
                }
            }
            CellIndex::compact(&mut cells)
                .map_err(|e| invalid(format!("cannot compact cells: {e}")))?;
            Ok(addresses(cells))
        }
        ("cell_uncompact", [cells, resolution]) => {
            let cells = list(cells)?;
            let resolution = super::resolution(resolution)?;
            if cells.iter().any(|cell| cell.resolution() > resolution) {
                return Err(invalid(
                    "uncompact resolution must not be coarser than any input cell",
                ));
            }
            let count = CellIndex::uncompact_size(cells.iter().copied(), resolution);
            if count > MAX_CELLS {
                return Err(too_many(count));
            }
            Ok(addresses(CellIndex::uncompact(cells, resolution).collect()))
        }
        _ => Err(invalid("invalid function or argument count")),
    }
}

/// Parse a canonical lowercase H3 cell address.
pub(super) fn parse(value: &Value) -> Result<CellIndex> {
    let Value::String(address) = value else {
        return Err(invalid("expected an H3 cell address string"));
    };
    let cell = address
        .parse::<CellIndex>()
        .map_err(|_| invalid("invalid H3 cell address"))?;
    if cell.to_string() != *address {
        return Err(invalid(
            "cell address must use canonical lowercase hexadecimal",
        ));
    }
    Ok(cell)
}

fn list(value: &Value) -> Result<Vec<CellIndex>> {
    let Value::Array(items) = value else {
        return Err(invalid("expected an array of H3 cell addresses"));
    };
    if items.len() as u64 > MAX_CELLS {
        return Err(too_many(items.len() as u64));
    }
    let mut cells = items.iter().map(parse).collect::<Result<Vec<_>>>()?;
    cells.sort_unstable_by_key(|cell| u64::from(*cell));
    cells.dedup();
    Ok(cells)
}

fn addresses(mut cells: Vec<CellIndex>) -> Value {
    cells.sort_unstable_by_key(|cell| u64::from(*cell));
    cells.dedup();
    Value::Array(
        cells
            .into_iter()
            .map(|cell| Value::String(cell.to_string()))
            .collect(),
    )
}

/// Outline a cell with straight longitude/latitude edges. A cell crossing the
/// antimeridian becomes a MultiPolygon split at longitude 180; a cell that
/// contains a pole has no such outline and is rejected.
fn boundary(cell: CellIndex) -> Result<Value> {
    let vertices = cell
        .boundary()
        .iter()
        .map(|ll| (ll.lng(), ll.lat()))
        .collect::<Vec<Position>>();
    if vertices.iter().any(|p| p.1.abs() == 90.0) {
        return Err(invalid("cell boundary touches a pole"));
    }
    let mut unwrapped = vec![vertices[0]];
    for p in &vertices[1..] {
        let previous = unwrapped[unwrapped.len() - 1].0;
        unwrapped.push((previous + wrapped(p.0 - previous), p.1));
    }
    let last = unwrapped[unwrapped.len() - 1].0;
    if (last + wrapped(vertices[0].0 - last) - unwrapped[0].0).abs() > 180.0 {
        return Err(invalid(
            "cell contains a pole; its boundary is not a supported polygon",
        ));
    }
    let east = unwrapped
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let west = unwrapped.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let (kind, polygons) = if east > 180.0 || west < -180.0 {
        let seam = if east > 180.0 { 180.0 } else { -180.0 };
        let shift = if east > 180.0 { -360.0 } else { 360.0 };
        let inner = clip(&unwrapped, seam, seam > 0.0);
        let outer = clip(&unwrapped, seam, seam < 0.0)
            .into_iter()
            .map(|p| (p.0 + shift, p.1))
            .collect::<Vec<_>>();
        let pieces = [inner, outer]
            .into_iter()
            .filter(|ring| ring.len() >= 3)
            .map(|ring| vec![closed(ring)])
            .collect::<Vec<_>>();
        (Kind::MultiPolygon, pieces)
    } else {
        (Kind::Polygon, vec![vec![closed(unwrapped)]])
    };
    let region = Region::new(kind, polygons)
        .map_err(|e| invalid(format!("cell boundary is not a valid polygon: {e}")))?;
    Ok(region.to_value())
}

fn wrapped(delta: f64) -> f64 {
    if delta > 180.0 {
        delta - 360.0
    } else if delta < -180.0 {
        delta + 360.0
    } else {
        delta
    }
}

fn closed(mut ring: Vec<Position>) -> Vec<Position> {
    ring.push(ring[0]);
    ring
}

/// Keep the part of a polygon west (or east) of a meridian.
fn clip(points: &[Position], seam: f64, keep_west: bool) -> Vec<Position> {
    let keep = |p: Position| if keep_west { p.0 <= seam } else { p.0 >= seam };
    let crossing =
        |a: Position, b: Position| (seam, a.1 + (seam - a.0) * (b.1 - a.1) / (b.0 - a.0));
    let mut out: Vec<Position> = Vec::new();
    for (i, &current) in points.iter().enumerate() {
        let previous = points[(i + points.len() - 1) % points.len()];
        match (keep(previous), keep(current)) {
            (true, true) => out.push(current),
            (true, false) => out.push(crossing(previous, current)),
            (false, true) => {
                out.push(crossing(previous, current));
                out.push(current);
            }
            (false, false) => {}
        }
    }
    out.dedup();
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use h3o::Resolution;

    fn text(value: &str) -> Value {
        Value::String(value.into())
    }

    #[test]
    fn hierarchy_disk_distance_and_area_follow_h3() {
        let cell = text("8a1fb46622dffff");
        assert_eq!(
            call("cell_resolution", &[cell.clone()]).unwrap(),
            Value::Integer(10)
        );
        let parent = call("cell_parent", &[cell.clone(), Value::Integer(9)]).unwrap();
        let Value::String(parent) = parent else {
            panic!()
        };
        let children = call("cell_children", &[text(&parent), Value::Integer(10)]).unwrap();
        let Value::Array(children) = children else {
            panic!()
        };
        assert_eq!(children.len(), 7);
        assert!(children.contains(&cell));
        let compacted = call("cell_compact", &[Value::Array(children.clone())]).unwrap();
        assert_eq!(compacted, Value::Array(vec![text(&parent)]));
        let expanded = call("cell_uncompact", &[compacted, Value::Integer(10)]).unwrap();
        assert_eq!(expanded, Value::Array(children));
        let Value::Array(disk) = call("cell_disk", &[cell.clone(), Value::Integer(2)]).unwrap()
        else {
            panic!()
        };
        assert_eq!(disk.len(), 19);
        assert_eq!(
            call("cell_distance", &[cell.clone(), text("8a1fb46622d7fff")]).unwrap(),
            Value::Integer(1)
        );
        let Value::Number(area) = call("cell_area", &[cell]).unwrap() else {
            panic!()
        };
        assert!((10_000.0..20_000.0).contains(&area), "{area}");
    }

    #[test]
    fn rejects_invalid_cells_resolutions_and_oversized_results() {
        let cell = text("8a1fb46622dffff");
        assert!(call("cell_parent", &[cell.clone(), Value::Integer(11)]).is_err());
        assert!(call("cell_children", &[cell.clone(), Value::Integer(9)]).is_err());
        assert_eq!(
            call(
                "cell_children",
                &[text("8001fffffffffff"), Value::Integer(15)]
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            call("cell_disk", &[cell.clone(), Value::Integer(100)])
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        assert!(call("cell_disk", &[cell.clone(), Value::Number(1.5)]).is_err());
        assert!(call("cell_resolution", &[text("8A1FB46622DFFFF")]).is_err());
        assert!(call("cell_resolution", &[Value::Integer(1)]).is_err());
        assert!(call(
            "cell_compact",
            &[Value::Array(vec![cell, text("891fb46622fffff")])]
        )
        .is_err());
    }

    #[test]
    fn boundaries_are_valid_and_split_at_the_antimeridian() {
        let ordinary = call("cell_boundary", &[text("8a1fb46622dffff")]).unwrap();
        let Value::Object(fields) = &ordinary else {
            panic!()
        };
        assert_eq!(fields["type"], text("Polygon"));
        let center = h3o::LatLng::new(0.0, 180.0)
            .unwrap()
            .to_cell(Resolution::Two);
        let seam = call("cell_boundary", &[text(&center.to_string())]).unwrap();
        let Value::Object(fields) = &seam else {
            panic!()
        };
        assert_eq!(fields["type"], text("MultiPolygon"));
        let polar = h3o::LatLng::new(90.0, 0.0)
            .unwrap()
            .to_cell(Resolution::Zero);
        assert!(call("cell_boundary", &[text(&polar.to_string())]).is_err());
        let mut checked = 0;
        for base in CellIndex::base_cells() {
            for cell in base.children(Resolution::One) {
                if base.boundary().iter().all(|ll| ll.lat().abs() < 80.0) {
                    call("cell_boundary", &[text(&cell.to_string())]).unwrap();
                    checked += 1;
                }
            }
        }
        assert!(checked > 400, "{checked}");
    }
}
