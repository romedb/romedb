//! Morton (Z-order) keys over quantized longitude/latitude, and conservative
//! covers of query areas by ordered key ranges.
//!
//! Each axis is quantized to 31 bits, so a key fits a positive SQLite integer
//! and preserves order. Quantization is a composition of monotone binary64
//! operations, so a coordinate inside a closed interval always quantizes inside
//! the quantized interval. Covers may contain extra keys; they never omit one.
use super::exact::{orientation, Position};
use super::geometry::{Location, Region};
use std::cmp::Ordering;

pub(crate) const BITS: u32 = 31;
const CELLS: u64 = 1 << BITS;
const LONGITUDE_QUANTUM: f64 = 360.0 / CELLS as f64;
const LATITUDE_QUANTUM: f64 = 180.0 / CELLS as f64;

pub(crate) fn quantize_longitude(longitude: f64) -> u32 {
    quantize((longitude + 180.0) / 360.0)
}

pub(crate) fn quantize_latitude(latitude: f64) -> u32 {
    quantize((latitude + 90.0) / 180.0)
}

fn quantize(fraction: f64) -> u32 {
    let cell = (fraction * CELLS as f64).floor();
    if cell <= 0.0 {
        0
    } else {
        (cell as u64).min(CELLS - 1) as u32
    }
}

pub(crate) fn key(point: Position) -> i64 {
    interleave(quantize_longitude(point.0), quantize_latitude(point.1)) as i64
}

fn interleave(x: u32, y: u32) -> u64 {
    spread(x) | (spread(y) << 1)
}

fn spread(value: u32) -> u64 {
    let mut v = value as u64;
    v = (v | (v << 16)) & 0x0000_FFFF_0000_FFFF;
    v = (v | (v << 8)) & 0x00FF_00FF_00FF_00FF;
    v = (v | (v << 4)) & 0x0F0F_0F0F_0F0F_0F0F;
    v = (v | (v << 2)) & 0x3333_3333_3333_3333;
    v = (v | (v << 1)) & 0x5555_5555_5555_5555;
    v
}

/// Inclusive rectangle of quantized cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) x0: u32,
    pub(crate) x1: u32,
    pub(crate) y0: u32,
    pub(crate) y1: u32,
}

impl Window {
    /// Quantized cells of a closed longitude/latitude rectangle, `west <= east`.
    pub(crate) fn from_degrees(west: f64, south: f64, east: f64, north: f64) -> Self {
        Self {
            x0: quantize_longitude(west),
            x1: quantize_longitude(east),
            y0: quantize_latitude(south),
            y1: quantize_latitude(north),
        }
    }
}

/// Quadtree cell: `level` 0 is the whole grid, `level` 31 one quantized cell.
#[derive(Clone, Copy, Debug)]
struct Cell {
    level: u32,
    x: u32,
    y: u32,
}

impl Cell {
    fn size(&self) -> u64 {
        1u64 << (BITS - self.level)
    }
    fn range(&self) -> (i64, i64) {
        let first = interleave(self.x, self.y);
        let span = self.size() * self.size();
        (first as i64, (first + span - 1) as i64)
    }
    fn children(&self) -> [Cell; 4] {
        let half = (self.size() / 2) as u32;
        let level = self.level + 1;
        [
            Cell {
                level,
                x: self.x,
                y: self.y,
            },
            Cell {
                level,
                x: self.x + half,
                y: self.y,
            },
            Cell {
                level,
                x: self.x,
                y: self.y + half,
            },
            Cell {
                level,
                x: self.x + half,
                y: self.y + half,
            },
        ]
    }
    fn last_x(&self) -> u32 {
        (self.x as u64 + self.size() - 1) as u32
    }
    fn last_y(&self) -> u32 {
        (self.y as u64 + self.size() - 1) as u32
    }
    /// Longitude/latitude rectangle holding every point quantized into this
    /// cell, widened by one quantum against rounding at its edges.
    fn degrees(&self) -> (f64, f64, f64, f64) {
        let size = self.size() as f64;
        (
            self.x as f64 * LONGITUDE_QUANTUM - 180.0 - LONGITUDE_QUANTUM,
            self.y as f64 * LATITUDE_QUANTUM - 90.0 - LATITUDE_QUANTUM,
            (self.x as f64 + size) * LONGITUDE_QUANTUM - 180.0 + LONGITUDE_QUANTUM,
            (self.y as f64 + size) * LATITUDE_QUANTUM - 90.0 + LATITUDE_QUANTUM,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlap {
    Outside,
    Partial,
    Inside,
}

/// Cover of several windows by at most `budget` key ranges.
pub(crate) fn cover_windows(windows: &[Window], budget: usize) -> Vec<(i64, i64)> {
    cover(budget, (), |cell: &Cell, _: &()| {
        let overlap = windows
            .iter()
            .map(|w| overlap_window(cell, w))
            .max_by_key(|o| match o {
                Overlap::Outside => 0,
                Overlap::Partial => 1,
                Overlap::Inside => 2,
            })
            .unwrap_or(Overlap::Outside);
        (overlap, ())
    })
}

/// Cover of the points that a region covers, including its boundary. Each
/// partial cell keeps the edges that cross it, so a child only tests the
/// edges of its parent.
pub(crate) fn cover_region(region: &Region, budget: usize) -> Vec<(i64, i64)> {
    let all = (0..region.edges().len() as u32).collect::<Vec<_>>();
    cover(budget, all, |cell: &Cell, edges: &Vec<u32>| {
        overlap_region(cell, region, edges)
    })
}

/// Level-tagged code of a quadtree cell: `4^level + prefix`, where the prefix
/// is the cell's Morton key truncated to `2 * level` bits. Codes of different
/// levels never collide, and every code fits a positive SQLite integer.
fn cell_code(cell: &Cell) -> i64 {
    let prefix = interleave(cell.x, cell.y) >> (2 * (BITS - cell.level));
    ((1u64 << (2 * cell.level)) + prefix) as i64
}

/// Codes of the 32 cells, one per level, that contain a point. A region whose
/// cover includes any of these cells may contain the point.
pub(crate) fn ancestor_codes(point: Position) -> Vec<i64> {
    let key = key(point) as u64;
    (0..=BITS)
        .map(|level| ((1u64 << (2 * level)) + (key >> (2 * (BITS - level)))) as i64)
        .collect()
}

/// Codes of at most `budget` cells that together contain every point the region
/// covers. Fully covered cells stop descending; partial cells descend while the
/// budget allows.
pub(crate) fn region_cells(region: &Region, budget: usize) -> Vec<i64> {
    let all = (0..region.edges().len() as u32).collect::<Vec<_>>();
    let mut cells = Vec::new();
    let mut partial = vec![(
        Cell {
            level: 0,
            x: 0,
            y: 0,
        },
        all,
    )];
    while !partial.is_empty() {
        let level = partial[0].0.level;
        if level == BITS || cells.len() + 4 * partial.len() > budget.max(1) {
            cells.extend(partial.iter().map(|(cell, _)| cell_code(cell)));
            break;
        }
        let mut next = Vec::new();
        for (parent, edges) in &partial {
            for cell in parent.children() {
                match overlap_region(&cell, region, edges) {
                    (Overlap::Outside, _) => {}
                    (Overlap::Inside, _) => cells.push(cell_code(&cell)),
                    (Overlap::Partial, crossing) => next.push((cell, crossing)),
                }
            }
        }
        partial = next;
    }
    cells.sort_unstable();
    cells
}

/// Merge the ranges separated by the smallest key gaps until at most `arms`
/// remain. Each merge reads the entries in one gap, which lie near the query
/// area because nearby keys are nearby cells; in exchange the statement
/// compiles and seeks fewer ranges.
pub(crate) fn coalesce(mut ranges: Vec<(i64, i64)>, arms: usize) -> Vec<(i64, i64)> {
    while ranges.len() > arms.max(1) {
        let (i, _) = ranges
            .windows(2)
            .enumerate()
            .min_by_key(|(_, pair)| pair[1].0 - pair[0].1)
            .expect("at least two ranges");
        ranges[i].1 = ranges[i + 1].1;
        ranges.remove(i + 1);
    }
    ranges
}

/// Sort ranges and merge those that touch or overlap.
pub(crate) fn merge(mut ranges: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    ranges.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match merged.last_mut() {
            Some(previous) if previous.1 + 1 >= first => previous.1 = previous.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

fn overlap_window(cell: &Cell, w: &Window) -> Overlap {
    if cell.last_x() < w.x0 || cell.x > w.x1 || cell.last_y() < w.y0 || cell.y > w.y1 {
        Overlap::Outside
    } else if w.x0 <= cell.x && cell.last_x() <= w.x1 && w.y0 <= cell.y && cell.last_y() <= w.y1 {
        Overlap::Inside
    } else {
        Overlap::Partial
    }
}

fn overlap_region(cell: &Cell, region: &Region, parent: &[u32]) -> (Overlap, Vec<u32>) {
    let (west, south, east, north) = cell.degrees();
    let bounds = region.bounds();
    if east < bounds.west || west > bounds.east || north < bounds.south || south > bounds.north {
        return (Overlap::Outside, Vec::new());
    }
    let corners = [(west, south), (east, south), (east, north), (west, north)];
    let edges = region.edges();
    let crossing = parent
        .iter()
        .copied()
        .filter(|&i| {
            let (a, b) = edges[i as usize];
            a.0.max(b.0) >= west
                && a.0.min(b.0) <= east
                && a.1.max(b.1) >= south
                && a.1.min(b.1) <= north
                && {
                    let sides = corners.map(|c| orientation(a, b, c));
                    !(sides.iter().all(|s| *s == Ordering::Greater)
                        || sides.iter().all(|s| *s == Ordering::Less))
                }
        })
        .collect::<Vec<_>>();
    if !crossing.is_empty() {
        return (Overlap::Partial, crossing);
    }
    match region.locate(((west + east) / 2.0, (south + north) / 2.0)) {
        Location::Outside => (Overlap::Outside, Vec::new()),
        _ => (Overlap::Inside, Vec::new()),
    }
}

/// Descend the quadtree one level at a time. When expanding every partial cell
/// would exceed the budget, emit the partial cells whole instead. The result is
/// sorted with adjacent ranges merged.
fn cover<S>(
    budget: usize,
    root: S,
    classify: impl Fn(&Cell, &S) -> (Overlap, S),
) -> Vec<(i64, i64)> {
    let mut ranges = Vec::new();
    let mut partial = vec![(
        Cell {
            level: 0,
            x: 0,
            y: 0,
        },
        root,
    )];
    while !partial.is_empty() {
        let level = partial[0].0.level;
        if level == BITS || ranges.len() + 4 * partial.len() > budget.max(1) {
            ranges.extend(partial.iter().map(|(cell, _)| cell.range()));
            break;
        }
        let mut next = Vec::new();
        for (parent, state) in &partial {
            for cell in parent.children() {
                match classify(&cell, state) {
                    (Overlap::Outside, _) => {}
                    (Overlap::Inside, _) => ranges.push(cell.range()),
                    (Overlap::Partial, child) => next.push((cell, child)),
                }
            }
        }
        partial = next;
    }
    merge(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(ranges: &[(i64, i64)], key: i64) -> bool {
        ranges.iter().any(|(a, b)| *a <= key && key <= *b)
    }

    struct Random(u64);
    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn keys_preserve_axis_order_and_fit_positive_integers() {
        assert_eq!(key((-180.0, -90.0)), 0);
        assert_eq!(key((180.0, 90.0)), (1i64 << 62) - 1);
        assert_eq!(
            quantize_longitude(180.0),
            quantize_longitude(179.99999999999997)
        );
        assert!(key((0.0, 0.0)) > 0);
        let mut random = Random(7);
        for _ in 0..10_000 {
            let x = random.next() as u32 >> 1;
            let y = random.next() as u32 >> 1;
            let k = interleave(x, y);
            let (mut rx, mut ry) = (0u32, 0u32);
            for bit in 0..BITS {
                rx |= (((k >> (2 * bit)) & 1) as u32) << bit;
                ry |= (((k >> (2 * bit + 1)) & 1) as u32) << bit;
            }
            assert_eq!((rx, ry), (x, y));
        }
    }

    #[test]
    fn quantization_is_monotone_at_interval_endpoints() {
        let mut random = Random(11);
        for _ in 0..100_000 {
            let a = random.unit() * 360.0 - 180.0;
            let b = f64::from_bits(a.to_bits() + 1);
            assert!(quantize_longitude(a) <= quantize_longitude(b));
            let c = random.unit() * 180.0 - 90.0;
            let d = f64::from_bits(c.to_bits().wrapping_add(1));
            if d.is_finite() && d <= 90.0 && d > c {
                assert!(quantize_latitude(c) <= quantize_latitude(d));
            }
        }
    }

    #[test]
    fn window_covers_include_every_point_and_respect_budget() {
        let mut random = Random(3);
        for budget in [1, 4, 16, 64] {
            for _ in 0..200 {
                let mut w = [random.unit() * 360.0 - 180.0, random.unit() * 360.0 - 180.0];
                w.sort_by(f64::total_cmp);
                let span = random.unit().powi(6) * 90.0;
                let south = random.unit() * (180.0 - span) - 90.0;
                let window = Window::from_degrees(
                    w[0],
                    south,
                    w[0] + (w[1] - w[0]) * random.unit().powi(4),
                    south + span,
                );
                let ranges = cover_windows(&[window], budget);
                assert!(
                    ranges.len() <= budget.max(4) + 4,
                    "{} ranges for budget {budget}",
                    ranges.len()
                );
                for _ in 0..200 {
                    let x = window.x0 as u64
                        + random.next() % (window.x1 as u64 - window.x0 as u64 + 1);
                    let y = window.y0 as u64
                        + random.next() % (window.y1 as u64 - window.y0 as u64 + 1);
                    let k = interleave(x as u32, y as u32) as i64;
                    assert!(covered(&ranges, k), "{window:?} {x} {y}");
                }
                for corner in [
                    (window.x0, window.y0),
                    (window.x1, window.y1),
                    (window.x0, window.y1),
                    (window.x1, window.y0),
                ] {
                    assert!(covered(&ranges, interleave(corner.0, corner.1) as i64));
                }
            }
        }
    }

    #[test]
    fn region_cells_contain_every_covered_point_and_respect_budget() {
        let text = r#"{"type":"MultiPolygon","coordinates":[
            [[[100,0],[100.001,0],[101,1],[100.999,1.0005],[100,0]]],
            [[[170,-10],[180,-10],[180,10],[170,10],[170,-10]],[[172,-2],[174,-2],[174,2],[172,2],[172,-2]]]]}"#;
        let super::super::geometry::Geometry::Region(region) =
            super::super::geometry::Geometry::from_geojson(text).unwrap()
        else {
            panic!()
        };
        let mut random = Random(9);
        for budget in [4, 16, 32] {
            let cells = region_cells(&region, budget);
            assert!(
                cells.len() <= budget,
                "{} cells for budget {budget}",
                cells.len()
            );
            let mut checked = 0;
            for _ in 0..100_000 {
                let p = if random.next() % 2 == 0 {
                    (100.0 + random.unit(), random.unit())
                } else {
                    (170.0 + random.unit() * 10.0, random.unit() * 20.0 - 10.0)
                };
                if region.locate(p) != Location::Outside {
                    checked += 1;
                    let codes = ancestor_codes(p);
                    assert!(
                        codes.iter().any(|c| cells.binary_search(c).is_ok()),
                        "{p:?} budget {budget}"
                    );
                }
            }
            for (p, _) in region.edges() {
                assert!(ancestor_codes(*p)
                    .iter()
                    .any(|c| cells.binary_search(c).is_ok()));
            }
            assert!(checked > 1000);
        }
        assert_eq!(ancestor_codes((0.0, 0.0)).len(), 32);
        assert_eq!(ancestor_codes((-180.0, -90.0))[0], 1);
        assert!(ancestor_codes((180.0, 90.0)).iter().all(|c| *c > 0));
    }

    #[test]
    fn region_covers_include_every_covered_point() {
        let text = r#"{"type":"MultiPolygon","coordinates":[
            [[[100,0],[100.001,0],[101,1],[100.999,1.0005],[100,0]]],
            [[[170,-10],[180,-10],[180,10],[170,10],[170,-10]],[[172,-2],[174,-2],[174,2],[172,2],[172,-2]]]]}"#;
        let super::super::geometry::Geometry::Region(region) =
            super::super::geometry::Geometry::from_geojson(text).unwrap()
        else {
            panic!()
        };
        let mut random = Random(5);
        for budget in [8, 64, 256] {
            let ranges = cover_region(&region, budget);
            let mut inside = 0;
            for _ in 0..200_000 {
                let p = if random.next() % 2 == 0 {
                    (100.0 + random.unit() * 1.0, random.unit() * 1.0)
                } else {
                    (170.0 + random.unit() * 10.0, random.unit() * 20.0 - 10.0)
                };
                if region.locate(p) != Location::Outside {
                    inside += 1;
                    assert!(covered(&ranges, key(p)), "{p:?} budget {budget}");
                }
            }
            for (p, _) in region.edges() {
                assert!(covered(&ranges, key(*p)), "{p:?} budget {budget}");
            }
            assert!(inside > 1000);
        }
        let precise = cover_region(&region, 256);
        let width: i64 = precise.iter().map(|(a, b)| b - a + 1).sum();
        let coarse: i64 = cover_region(&region, 8)
            .iter()
            .map(|(a, b)| b - a + 1)
            .sum();
        assert!(width < coarse);
    }
}
