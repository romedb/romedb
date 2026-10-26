//! S1 candidate-index evaluation. Compares the released latitude index, Morton
//! range covers and an H3 cell prototype against exhaustive references, over
//! in-memory sorted keys so the comparison measures index work, not storage.
use super::exact::Position;
use super::geometry::{Geometry, Location, Region};
use super::morton;
use std::collections::BTreeSet;

const BUDGET: usize = 64;
const H3_RESOLUTIONS: [u8; 3] = [5, 7, 9];

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
    fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-300);
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

fn uniform(random: &mut Random) -> Position {
    (
        random.unit() * 360.0 - 180.0,
        (2.0 * random.unit() - 1.0).asin().to_degrees(),
    )
}

fn clamp(p: Position) -> Position {
    let mut longitude = p.0;
    if longitude > 180.0 {
        longitude -= 360.0;
    }
    if longitude < -180.0 {
        longitude += 360.0;
    }
    (longitude, p.1.clamp(-89.9, 89.9))
}

fn cities(random: &mut Random, count: usize) -> Vec<Position> {
    (0..count)
        .map(|_| (random.unit() * 300.0 - 150.0, random.unit() * 120.0 - 60.0))
        .collect()
}

fn city_points(random: &mut Random, centers: &[Position], n: usize, sigma: f64) -> Vec<Position> {
    (0..n)
        .map(|i| {
            let c = centers[i % centers.len()];
            clamp((c.0 + random.normal() * sigma, c.1 + random.normal() * sigma))
        })
        .collect()
}

struct Indexes {
    points: Vec<Position>,
    morton: Vec<(i64, u32)>,
    latitude: Vec<(f64, u32)>,
    h3: Vec<Vec<(u64, u32)>>,
}

impl Indexes {
    fn new(points: Vec<Position>) -> Self {
        let mut morton = points
            .iter()
            .enumerate()
            .map(|(i, p)| (morton::key(*p), i as u32))
            .collect::<Vec<_>>();
        morton.sort_unstable();
        let mut latitude = points
            .iter()
            .enumerate()
            .map(|(i, p)| (p.1, i as u32))
            .collect::<Vec<_>>();
        latitude.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let h3 = H3_RESOLUTIONS
            .iter()
            .map(|res| {
                let resolution = h3o::Resolution::try_from(*res).unwrap();
                let mut cells = points
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (u64::from(cell(*p, resolution)), i as u32))
                    .collect::<Vec<_>>();
                cells.sort_unstable();
                cells
            })
            .collect();
        Self {
            points,
            morton,
            latitude,
            h3,
        }
    }

    fn morton_candidates(&self, ranges: &[(i64, i64)]) -> Vec<u32> {
        let mut out = Vec::new();
        for (first, last) in ranges {
            let start = self.morton.partition_point(|(k, _)| k < first);
            let end = self.morton.partition_point(|(k, _)| k <= last);
            out.extend(self.morton[start..end].iter().map(|(_, i)| *i));
        }
        out
    }

    fn latitude_candidates(&self, south: f64, north: f64) -> Vec<u32> {
        let start = self.latitude.partition_point(|(l, _)| *l < south);
        let end = self.latitude.partition_point(|(l, _)| *l <= north);
        self.latitude[start..end].iter().map(|(_, i)| *i).collect()
    }

    fn h3_candidates(&self, center: Position, radius: f64) -> (Vec<u32>, usize) {
        let mut choice = 0;
        for (slot, res) in H3_RESOLUTIONS.iter().enumerate() {
            if h3_k(radius, *res) <= 10 {
                choice = slot;
            }
        }
        let resolution = h3o::Resolution::try_from(H3_RESOLUTIONS[choice]).unwrap();
        let k = h3_k(radius, H3_RESOLUTIONS[choice]);
        let disk = cell(center, resolution).grid_disk::<Vec<_>>(k);
        let table = &self.h3[choice];
        let mut out = Vec::new();
        for c in &disk {
            let key = u64::from(*c);
            let start = table.partition_point(|(k, _)| *k < key);
            let end = table.partition_point(|(k, _)| *k <= key);
            out.extend(table[start..end].iter().map(|(_, i)| *i));
        }
        (out, disk.len())
    }
}

fn cell(p: Position, resolution: h3o::Resolution) -> h3o::CellIndex {
    h3o::LatLng::new(p.1, p.0).unwrap().to_cell(resolution)
}

fn h3_k(radius: f64, res: u8) -> u32 {
    let edge = h3o::Resolution::try_from(res).unwrap().edge_length_m();
    ((radius + 2.0 * edge) / (1.3 * edge)).ceil() as u32 + 1
}

#[derive(Default)]
struct Tally {
    queries: u64,
    candidates: u64,
    seeks: u64,
    results: u64,
    worst: u64,
    misses: u64,
    rounds: u64,
}

impl Tally {
    fn add(&mut self, candidates: &[u32], seeks: usize, expected: &BTreeSet<u32>) {
        let found = candidates.iter().copied().collect::<BTreeSet<_>>();
        self.queries += 1;
        self.candidates += candidates.len() as u64;
        self.worst = self.worst.max(candidates.len() as u64);
        self.seeks += seeks as u64;
        self.results += expected.len() as u64;
        self.misses += expected.difference(&found).count() as u64;
    }
    fn line(&self, workload: &str, query: &str, method: &str) -> String {
        let q = self.queries.max(1);
        format!(
            "| {workload} | {query} | {method} | {} | {} | {} | {} | {} |{}",
            self.results / q,
            self.candidates / q,
            self.worst,
            self.seeks / q,
            self.misses,
            if self.rounds > 0 {
                format!(" rounds {:.1}", self.rounds as f64 / q as f64)
            } else {
                String::new()
            }
        )
    }
}

fn exhaustive(points: &[Position], keep: impl Fn(Position) -> bool) -> BTreeSet<u32> {
    points
        .iter()
        .enumerate()
        .filter(|(_, p)| keep(**p))
        .map(|(i, _)| i as u32)
        .collect()
}

fn thin_band(c: Position) -> Region {
    let text = format!(
        r#"{{"type":"Polygon","coordinates":[[[{},{}],[{},{}],[{},{}],[{},{}],[{},{}]]]}}"#,
        c.0,
        c.1,
        c.0 + 0.002,
        c.1,
        c.0 + 0.502,
        c.1 + 0.5,
        c.0 + 0.5,
        c.1 + 0.5,
        c.0,
        c.1
    );
    match Geometry::from_geojson(&text).unwrap() {
        Geometry::Region(r) => r,
        Geometry::Point(_) => unreachable!(),
    }
}

fn nearest(indexes: &Indexes, center: Position, k: usize, tally: &mut Tally) {
    let mut expected = indexes
        .points
        .iter()
        .enumerate()
        .map(|(i, p)| (super::distance(center, *p), i as u32))
        .collect::<Vec<_>>();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    expected.truncate(k);
    let mut radius = 100.0;
    let mut total = Vec::new();
    let mut seeks = 0;
    let mut rounds = 0;
    let best = loop {
        rounds += 1;
        let ranges = morton::cover_windows(&super::radius_windows(center, radius), BUDGET);
        seeks += ranges.len();
        let candidates = indexes.morton_candidates(&ranges);
        let mut within = candidates
            .iter()
            .map(|i| (super::distance(center, indexes.points[*i as usize]), *i))
            .filter(|(d, _)| *d <= radius)
            .collect::<Vec<_>>();
        total.extend(candidates);
        within.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        within.truncate(k);
        if within.len() == k || radius >= std::f64::consts::PI * super::EARTH_RADIUS_M {
            break within;
        }
        radius *= 2.0;
    };
    tally.queries += 1;
    tally.candidates += total.len() as u64;
    tally.worst = tally.worst.max(total.len() as u64);
    tally.seeks += seeks as u64;
    tally.results += best.len() as u64;
    tally.rounds += rounds;
    if best != expected {
        tally.misses += 1;
    }
}

fn evaluate(
    name: &str,
    points: Vec<Position>,
    queries: usize,
    random: &mut Random,
    out: &mut Vec<String>,
) {
    let indexes = Indexes::new(points);
    let center = |random: &mut Random, indexes: &Indexes| {
        let p = indexes.points[(random.next() % indexes.points.len() as u64) as usize];
        clamp((p.0 + random.normal() * 0.01, p.1 + random.normal() * 0.01))
    };
    for radius in [1_000.0, 5_000.0, 25_000.0] {
        let (mut lat, mut mor, mut h3) = (Tally::default(), Tally::default(), Tally::default());
        let (mut m8, mut m4) = (Tally::default(), Tally::default());
        for _ in 0..queries {
            let c = center(random, &indexes);
            let expected = exhaustive(&indexes.points, |p| super::distance(c, p) <= radius);
            let (south, north) = super::latitude_bounds(c.1, radius);
            lat.add(&indexes.latitude_candidates(south, north), 1, &expected);
            let ranges = morton::cover_windows(&super::radius_windows(c, radius), BUDGET);
            mor.add(&indexes.morton_candidates(&ranges), ranges.len(), &expected);
            for (arms, tally) in [(8, &mut m8), (4, &mut m4)] {
                let merged = morton::coalesce(ranges.clone(), arms);
                tally.add(&indexes.morton_candidates(&merged), merged.len(), &expected);
            }
            let (cells, seeks) = indexes.h3_candidates(c, radius);
            h3.add(&cells, seeks, &expected);
        }
        let query = format!("radius {} km", radius / 1000.0);
        out.push(lat.line(name, &query, "latitude"));
        out.push(mor.line(name, &query, "morton"));
        out.push(m8.line(name, &query, "morton 8 arms"));
        out.push(m4.line(name, &query, "morton 4 arms"));
        out.push(h3.line(name, &query, "h3 5/7/9"));
    }
    for size in [0.1, 1.0] {
        let (mut lat, mut mor) = (Tally::default(), Tally::default());
        for _ in 0..queries {
            let c = center(random, &indexes);
            let (west, south, east, north) = (
                c.0 - size / 2.0,
                (c.1 - size / 2.0).max(-90.0),
                c.0 + size / 2.0,
                (c.1 + size / 2.0).min(90.0),
            );
            let (west, east) = (
                if west < -180.0 { west + 360.0 } else { west },
                if east > 180.0 { east - 360.0 } else { east },
            );
            let expected = exhaustive(&indexes.points, |p| {
                super::in_bbox(p, west, south, east, north)
            });
            lat.add(&indexes.latitude_candidates(south, north), 1, &expected);
            let ranges =
                morton::cover_windows(&super::bbox_windows(west, south, east, north), BUDGET);
            mor.add(&indexes.morton_candidates(&ranges), ranges.len(), &expected);
        }
        let query = format!("viewport {size} deg");
        out.push(lat.line(name, &query, "latitude"));
        out.push(mor.line(name, &query, "morton"));
    }
    {
        let (west, south, east, north) = (179.5, -1.0, -179.5, 1.0);
        let expected = exhaustive(&indexes.points, |p| {
            super::in_bbox(p, west, south, east, north)
        });
        let mut lat = Tally::default();
        let mut mor = Tally::default();
        lat.add(&indexes.latitude_candidates(south, north), 1, &expected);
        let ranges = morton::cover_windows(&super::bbox_windows(west, south, east, north), BUDGET);
        mor.add(&indexes.morton_candidates(&ranges), ranges.len(), &expected);
        out.push(lat.line(name, "dateline viewport", "latitude"));
        out.push(mor.line(name, "dateline viewport", "morton"));
    }
    let (mut lat, mut boxed, mut shaped) = (Tally::default(), Tally::default(), Tally::default());
    for _ in 0..queries.min(50) {
        let c = center(random, &indexes);
        let c = (c.0.clamp(-179.0, 179.0), c.1.clamp(-88.0, 88.0));
        let region = thin_band(c);
        let expected = exhaustive(&indexes.points, |p| region.locate(p) != Location::Outside);
        let b = region.bounds();
        lat.add(&indexes.latitude_candidates(b.south, b.north), 1, &expected);
        let ranges = morton::cover_windows(
            &super::bbox_windows(b.west, b.south, b.east, b.north),
            BUDGET,
        );
        boxed.add(&indexes.morton_candidates(&ranges), ranges.len(), &expected);
        let ranges = morton::cover_region(&region, BUDGET);
        shaped.add(&indexes.morton_candidates(&ranges), ranges.len(), &expected);
    }
    out.push(lat.line(name, "thin diagonal polygon", "latitude"));
    out.push(boxed.line(name, "thin diagonal polygon", "morton bbox"));
    out.push(shaped.line(name, "thin diagonal polygon", "morton region"));
    for k in [5, 50] {
        let mut tally = Tally::default();
        for _ in 0..queries.min(50) {
            let c = center(random, &indexes);
            nearest(&indexes, c, k, &mut tally);
        }
        out.push(tally.line(name, &format!("nearest k={k}"), "morton expansion"));
    }
}

#[test]
#[ignore = "S1 evaluation; run with --profile romedb-production -- --ignored --nocapture"]
fn candidate_index_evaluation() {
    let mut random = Random(0x5eed);
    let mut out = vec![
        "| Workload | Query | Method | Results | Candidates | Worst | Seeks | Misses |".to_owned(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_owned(),
    ];
    let n = 200_000;
    let points = (0..n).map(|_| uniform(&mut random)).collect();
    evaluate("uniform 200k", points, 100, &mut random, &mut out);
    let centers = cities(&mut random, 20);
    let points = city_points(&mut random, &centers, n, 0.05);
    evaluate("20 cities 200k", points, 100, &mut random, &mut out);
    let points = (0..n)
        .map(|_| (random.unit() * 360.0 - 180.0, 10.0))
        .collect();
    evaluate("same latitude 200k", points, 100, &mut random, &mut out);
    let points = (0..2_000).map(|_| uniform(&mut random)).collect();
    evaluate("sparse 2k", points, 100, &mut random, &mut out);
    let points = vec![(106.7, 10.8); 10_000];
    evaluate("coincident 10k", points, 20, &mut random, &mut out);
    for line in &out {
        println!("{line}");
    }
    let city = (106.7, 10.8);
    println!("| Distant points | latitude 5 km | morton 5 km | results |");
    println!("| --- | --- | --- | --- |");
    for distant in [0usize, 100_000, 1_000_000] {
        let mut points = city_points(&mut random, &[city], 10_000, 0.05);
        points.extend((0..distant).map(|_| uniform(&mut random)));
        let indexes = Indexes::new(points);
        let radius = 5_000.0;
        let (south, north) = super::latitude_bounds(city.1, radius);
        let latitude = indexes.latitude_candidates(south, north).len();
        let ranges = morton::cover_windows(&super::radius_windows(city, radius), BUDGET);
        let candidates = indexes.morton_candidates(&ranges);
        let expected = exhaustive(&indexes.points, |p| super::distance(city, p) <= radius);
        let found = candidates.iter().copied().collect::<BTreeSet<_>>();
        assert!(expected.is_subset(&found));
        println!(
            "| {distant} | {latitude} | {} | {} |",
            candidates.len(),
            expected.len()
        );
    }
    assert!(out.iter().all(|line| !line.contains("morton")
        || line
            .split('|')
            .nth(8)
            .is_none_or(|m| m.trim() == "0" || m.trim() == "Misses")));
}

fn load(c: &crate::Connection, points: &[Position]) {
    c.execute("CREATE TABLE places", &crate::Parameters::new())
        .unwrap();
    for chunk in points.chunks(1000).enumerate() {
        let (start, chunk) = chunk;
        c.execute("BEGIN", &crate::Parameters::new()).unwrap();
        for (i, p) in chunk.iter().enumerate() {
            c.execute(
                &format!(
                    "INSERT INTO places {{id:type::record('places',{}),available:{},location:geo::point({},{})}}",
                    start * 1000 + i,
                    (start * 1000 + i) % 10 == 0,
                    p.0,
                    p.1
                ),
                &crate::Parameters::new(),
            )
            .unwrap();
        }
        c.execute("COMMIT", &crate::Parameters::new()).unwrap();
    }
}

fn measure(c: &crate::Connection, label: &str, queries: &[String], out: &mut Vec<String>) {
    let start = std::time::Instant::now();
    let (mut rows, mut read, mut seeks, mut steps, mut fullscan) = (0, 0, 0, 0, 0);
    for sql in queries {
        let profile = c.profile_select(sql, &crate::Parameters::new()).unwrap();
        rows += profile.result.rows.len() as u64;
        read += profile.metrics.rows_read;
        seeks += profile.metrics.btree_seeks;
        steps += profile.metrics.vm_steps;
        fullscan += profile.metrics.fullscan_steps;
    }
    let n = queries.len() as u64;
    out.push(format!(
        "| {label} | {:?} | {} | {} | {} | {} | {} |",
        start.elapsed() / queries.len() as u32,
        rows / n,
        read / n,
        seeks / n,
        steps / n,
        fullscan / n
    ));
}

#[test]
#[ignore = "SQL workload measurement; run with --profile romedb-production -- --ignored --nocapture"]
fn sql_workload_evaluation() {
    let mut random = Random(0xfa57);
    let n = 100_000;
    let centers = cities(&mut random, 20);
    let datasets = [
        (
            "uniform",
            (0..n).map(|_| uniform(&mut random)).collect::<Vec<_>>(),
        ),
        ("20 cities", city_points(&mut random, &centers, n, 0.05)),
        (
            "same latitude",
            (0..n)
                .map(|_| (random.unit() * 360.0 - 180.0, 10.0))
                .collect(),
        ),
    ];
    let mut out = vec![
        "| Workload and query | Mean latency | Rows | Rows read | B-tree seeks | VM steps | Full-scan steps |".to_owned(),
        "| --- | --- | --- | --- | --- | --- | --- |".to_owned(),
    ];
    for (name, points) in datasets {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let start = std::time::Instant::now();
        load(&c, &points);
        let loaded = start.elapsed();
        let start = std::time::Instant::now();
        c.execute(
            "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
            &crate::Parameters::new(),
        )
        .unwrap();
        out.push(format!(
            "| {name}: load {n} documents {loaded:?}, build index {:?} | | | | | | |",
            start.elapsed()
        ));
        let probes = (0..20)
            .map(|_| {
                let p = points[(random.next() % points.len() as u64) as usize];
                clamp((p.0 + random.normal() * 0.01, p.1 + random.normal() * 0.01))
            })
            .collect::<Vec<_>>();
        let each =
            |f: &dyn Fn(Position) -> String| probes.iter().map(|p| f(*p)).collect::<Vec<_>>();
        for format in ["morton", "latitude"] {
            if format == "latitude" {
                super::index_tests::downgrade_named(&c, "places");
            }
            for radius in [1_000, 5_000] {
                measure(
                    &c,
                    &format!("{name} {format} near {radius} m"),
                    &each(&|p| {
                        format!("SELECT id FROM search::near('places_location',geo::point({},{}),{radius})", p.0, p.1)
                    }),
                    &mut out,
                );
            }
            measure(
                &c,
                &format!("{name} {format} bbox 0.1 deg"),
                &each(&|p| {
                    format!(
                        "SELECT id FROM search::bbox('places_location',{},{},{},{})",
                        p.0 - 0.05,
                        (p.1 - 0.05).max(-90.0),
                        p.0 + 0.05,
                        (p.1 + 0.05).min(90.0)
                    )
                }),
                &mut out,
            );
            measure(
                &c,
                &format!("{name} {format} polygon 0.1 deg"),
                &each(&|p| {
                    format!("SELECT id FROM search::polygon('places_location',geo::envelope({},{},{},{}),'covers')", (p.0 - 0.05).max(-180.0), (p.1 - 0.05).max(-89.0), (p.0 + 0.05).min(180.0), (p.1 + 0.05).min(89.0))
                }),
                &mut out,
            );
            for k in [5, 50] {
                measure(
                    &c,
                    &format!("{name} {format} nearest k={k}"),
                    &each(&|p| {
                        format!("SELECT id FROM search::nearest('places_location',geo::point({},{}),{k})", p.0, p.1)
                    }),
                    &mut out,
                );
            }
        }
        measure(
            &c,
            &format!("{name} scan near 5000 m"),
            &each(&|p| {
                format!("SELECT id FROM places NOT INDEXED WHERE ST_DistanceSphere(location,ST_Point({},{})) <= 5000", p.0, p.1)
            }),
            &mut out,
        );
    }
    for line in out {
        println!("{line}");
    }
}

#[test]
#[ignore = "Filtered nearest measurement; run with --profile romedb-production -- --ignored --nocapture"]
fn nearest_filter_evaluation() {
    let mut random = Random(0x5eed);
    let n = 100_000;
    let centers = cities(&mut random, 20);
    let points = city_points(&mut random, &centers, n, 0.05);
    let db = crate::Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let none = crate::Parameters::new();
    c.execute("CREATE TABLE people", &none).unwrap();
    for (start, chunk) in points.chunks(1000).enumerate() {
        c.execute("BEGIN", &none).unwrap();
        for (i, p) in chunk.iter().enumerate() {
            let id = start * 1000 + i;
            let fleet = if random.next() % 1000 == 0 {
                "night"
            } else {
                "day"
            };
            c.execute(
                &format!(
                    "INSERT INTO people {{id:type::record('people',{id}),available:{},last_seen:{},fleet:'{fleet}',location:geo::point({},{})}}",
                    random.next() % 3 == 0,
                    random.next() % 100,
                    p.0,
                    p.1
                ),
                &none,
            )
            .unwrap();
        }
        c.execute("COMMIT", &none).unwrap();
    }
    c.execute(
        "CREATE SEARCH INDEX people_location ON people(location) USING SPATIAL",
        &none,
    )
    .unwrap();
    let probes = (0..20)
        .map(|_| {
            let p = points[(random.next() % points.len() as u64) as usize];
            clamp((p.0 + random.normal() * 0.01, p.1 + random.normal() * 0.01))
        })
        .collect::<Vec<_>>();
    let nearest = |filter: &str| {
        probes
            .iter()
            .map(|p| {
                let d = format!("ST_DistanceSphere(location, ST_Point({}, {}))", p.0, p.1);
                format!("SELECT id, {d} AS d FROM people WHERE {d} IS NOT NULL AND {filter} ORDER BY d, id LIMIT 5")
            })
            .collect::<Vec<_>>()
    };
    let mut out = vec![
        "| Query | Mean | Max | Rows | Rows read | Index steps | Full-scan steps | Rounds |"
            .to_owned(),
        "| --- | --- | --- | --- | --- | --- | --- | --- |".to_owned(),
    ];
    let run = |label: &str, queries: &[String], out: &mut Vec<String>| {
        let (mut rows, mut read, mut index, mut fullscan, mut rounds) = (0, 0, 0, 0, 0);
        let mut worst = std::time::Duration::ZERO;
        let start = std::time::Instant::now();
        for sql in queries {
            let one = std::time::Instant::now();
            let profile = c.profile_select(sql, &none).unwrap();
            worst = worst.max(one.elapsed());
            if std::env::var_os("ROMEDB_EVAL_DETAIL").is_some() {
                eprintln!(
                    "{label}: {:?} rounds={} ranges={} read={} index={} scan={}",
                    one.elapsed(),
                    profile.metrics.spatial_rounds,
                    profile.metrics.spatial_ranges,
                    profile.metrics.rows_read,
                    profile.metrics.index_steps,
                    profile.metrics.fullscan_steps
                );
            }
            rows += profile.result.rows.len() as u64;
            read += profile.metrics.rows_read;
            index += profile.metrics.index_steps;
            fullscan += profile.metrics.fullscan_steps;
            rounds += profile.metrics.spatial_rounds;
        }
        let q = queries.len() as u64;
        out.push(format!(
            "| {label} | {:?} | {worst:?} | {} | {} | {} | {} | {} |",
            start.elapsed() / q as u32,
            rows / q,
            read / q,
            index / q,
            fullscan / q,
            rounds / q
        ));
    };
    run(
        "available, fresh",
        &nearest("available = true AND last_seen >= 90"),
        &mut out,
    );
    run(
        "rare fleet, no fleet index",
        &nearest("fleet = 'night'"),
        &mut out,
    );
    run(
        "scan only: rare fleet",
        &probes
            .iter()
            .map(|p| format!("SELECT id FROM people NOT INDEXED WHERE fleet = 'night' AND ST_DistanceSphere(location, ST_Point({}, {})) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point({}, {})), id LIMIT 5", p.0, p.1, p.0, p.1))
            .collect::<Vec<_>>(),
        &mut out,
    );
    c.execute("CREATE INDEX people_fleet ON people(fleet)", &none)
        .unwrap();
    run(
        "rare fleet, fleet index",
        &nearest("fleet = 'night'"),
        &mut out,
    );
    run(
        "common fleet, fleet index",
        &nearest("fleet = 'day'"),
        &mut out,
    );
    for line in out {
        println!("{line}");
    }
}
