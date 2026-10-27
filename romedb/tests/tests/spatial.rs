use romedb::{Database, Parameters, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).expect(sql)
}

#[test]
fn spatial_distance_boundaries_and_types() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for (a, b, expected) in [
        ("0,0", "0,0", 0.0),
        ("180,0", "-180,0", 0.0),
        ("12,90", "-76,90", 0.0),
        ("12,-90", "-76,-90", 0.0),
        ("0,0", "1,0", 111_195.080_233_532_9),
        ("179,0", "-179,0", 222_390.160_467_065_8),
        ("0,0", "180,0", 20_015_114.442_035_925),
        ("12,90", "76,-90", 20_015_114.442_035_925),
    ] {
        for (a, b) in [(a, b), (b, a)] {
            let result = q(
                &c,
                &format!("SELECT geo::distance(geo::point({a}),geo::point({b})) AS d"),
            );
            let Value::Number(actual) = result.rows[0][0] else {
                panic!("distance type")
            };
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }
    assert_eq!(
        q(
            &c,
            "SELECT geo::within(geo::point(180,0),geo::point(-180,0),0)"
        )
        .rows,
        vec![vec![Value::Boolean(true)]]
    );
    assert_eq!(
        q(
            &c,
            "SELECT geo::within(geo::point(0,0),geo::point(1,0),100000)"
        )
        .rows,
        vec![vec![Value::Boolean(false)]]
    );
    assert_eq!(q(&c, "SELECT geo::within(geo::point(0,0),geo::point(1,0),geo::distance(geo::point(0,0),geo::point(1,0)))").rows,
        vec![vec![Value::Boolean(true)]]);
    let result = q(&c, "SELECT GEO::POINT(10,20)");
    assert!(result.columns[0].contains("geo::point"));
    assert!(matches!(result.rows[0][0], Value::Object(_)));
}

#[test]
fn spatial_invalid_inputs_and_failed_writes_are_atomic() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for expr in [
        "geo::point(181,0)",
        "geo::point(0,-91)",
        "geo::point('1',2)",
        "geo::point(NULL,2)",
        "geo::point(1)",
        "geo::point(1,2,3)",
        "geo::distance(1,2)",
        "geo::within(geo::point(0,0),geo::point(0,0),-1)",
        "geo::within(geo::point(0,0),geo::point(0,0),NULL)",
    ] {
        assert!(
            c.execute(&format!("SELECT {expr}"), &Parameters::new())
                .is_err(),
            "{expr}"
        );
        assert!(
            c.execute(
                &format!("INSERT INTO invalid {{location:{expr}}}"),
                &Parameters::new()
            )
            .is_err(),
            "{expr}"
        );
    }
    q(&c, "CREATE TABLE places");
    q(&c, "CREATE UNIQUE INDEX names ON places(name)");
    q(
        &c,
        "INSERT INTO places {id:places:a,name:'a',lon:1,location:geo::point(1,2)}",
    );
    q(
        &c,
        "INSERT INTO places {id:places:b,name:'b',lon:181,location:geo::point(2,2)}",
    );
    let before = q(&c, "SELECT * FROM places ORDER BY id").rows;
    q(&c, "BEGIN");
    assert!(c
        .execute(
            "UPDATE places SET location=geo::point(lon,2),name='changed'",
            &Parameters::new()
        )
        .is_err());
    assert_eq!(q(&c, "SELECT * FROM places ORDER BY id").rows, before);
    assert_eq!(
        c.lookup_index("places", "names", &Value::String("a".into()))
            .unwrap()
            .len(),
        1
    );
    q(&c, "ROLLBACK");
}

#[test]
fn spatial_points_persist_and_query_through_bound_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spatial.db");
    let expected;
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(
            &c,
            "INSERT INTO places {id:places:a,location:geo::point(106.7,10.8)}",
        );
        expected = q(&c, "SELECT location FROM places").rows;
        q(&c, "BEGIN");
        q(
            &c,
            "UPDATE places SET location=geo::point(0,0) RETURNING location",
        );
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(q(&c, "SELECT location FROM places").rows, expected);
    let params = Parameters::from([("$point".into(), expected[0][0].clone())]);
    assert_eq!(c.execute("SELECT geo::distance(location,$point) FROM places WHERE geo::within(location,$point,0)", &params).unwrap().rows,
        vec![vec![Value::Number(0.0)]]);
    for value in [
        Value::Null,
        Value::Number(f64::INFINITY),
        Value::Object(Default::default()),
        Value::Array(vec![Value::Integer(1), Value::Integer(2)]),
    ] {
        let params = Parameters::from([("$point".into(), value)]);
        assert!(c
            .execute("SELECT geo::distance(location,$point) FROM places", &params)
            .is_err());
    }
}

fn near(c: &romedb::Connection, center: &Value, radius: f64) -> Vec<Vec<Value>> {
    c.execute("SELECT id,distance_m FROM search::near('places_location',$point,$radius) ORDER BY distance_m,id",
        &Parameters::from([("$point".into(),center.clone()),("$radius".into(),Value::Number(radius))])).unwrap().rows
}

#[test]
fn spatial_index_lifecycle_reopen_and_statement_atomicity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("indexed.db");
    let center;
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(
            &c,
            "INSERT INTO places {id:places:a,location:geo::point(180,0),name:'a'}",
        );
        q(
            &c,
            "INSERT INTO places {id:places:b,location:null,name:'b'}",
        );
        q(&c, "INSERT INTO places {id:places:c,name:'c'}");
        q(&c, "DEFINE FIELD location ON places TYPE object NULLABLE");
        q(
            &c,
            "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
        );
        q(
            &c,
            "CREATE SEARCH INDEX IF NOT EXISTS places_location ON places(location) USING SPATIAL",
        );
        q(&c, "CREATE UNIQUE INDEX names ON places(name)");
        center = q(&c, "SELECT geo::point(-180,0)").rows[0][0].clone();
        assert_eq!(near(&c, &center, 0.0).len(), 1);
        q(&c, "BEGIN");
        q(
            &c,
            "UPDATE places SET location=geo::point(-180,0) WHERE id=places:b",
        );
        assert_eq!(near(&c, &center, 0.0).len(), 2);
        assert!(c
            .execute(
                "UPDATE places SET location=geo::point(1,2),name='duplicate'",
                &Parameters::new()
            )
            .is_err());
        assert_eq!(near(&c, &center, 0.0).len(), 2);
        q(&c, "DELETE FROM places:a");
        assert_eq!(near(&c, &center, 0.0).len(), 1);
        q(&c, "ROLLBACK");
        assert_eq!(near(&c, &center, 0.0).len(), 1);
        for sql in [
            "INSERT INTO places {location:{type:'Point',coordinates:[181,0]}}",
            "UPDATE places SET location=1",
            "DEFINE FIELD location ON places TYPE string OVERWRITE",
            "CREATE SEARCH INDEX IF NOT EXISTS names ON places(location) USING SPATIAL",
        ] {
            assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
        }
        let audit = c
            .check_collection_integrity("places", Default::default())
            .unwrap();
        assert_eq!(
            (audit.documents, audit.indexes, audit.index_entries),
            (3, 2, 6)
        );
        q(&c, "BEGIN");
        q(&c, "DROP INDEX places_location");
        assert!(c
            .execute(
                "SELECT * FROM search::near('places_location',geo::point(0,0),100)",
                &Parameters::new()
            )
            .is_err());
        q(&c, "ROLLBACK");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    assert_eq!(near(&c, &center, 0.0).len(), 1);
    c.check_collection_integrity("places", Default::default())
        .unwrap();
    q(&c, "DROP INDEX places_location");
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    assert_eq!(near(&c, &center, 0.0).len(), 1);
    q(&c, "DROP TABLE places");
    assert!(c
        .execute(
            "SELECT * FROM search::near('places_location',geo::point(0,0),100)",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn spatial_search_matches_scan_and_uses_index() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE places");
    q(&c, "BEGIN");
    // Deterministic coverage across longitude, latitude, poles and antimeridian.
    for i in 0..360 {
        let longitude = (i as f64) - 180.0;
        let latitude = ((i * 37) % 181) as f64 - 90.0;
        q(&c,&format!("INSERT INTO places {{id:type::record('places',{i}),location:geo::point({longitude},{latitude})}}"));
    }
    for (i, point) in [
        "180,0",
        "-180,0",
        "180,90",
        "-75,90",
        "42,-90",
        "-90,-90",
        "0,0",
        "0.00000000000001,0",
        "0,89.999999",
        "0,-89.999999",
    ]
    .iter()
    .enumerate()
    {
        q(
            &c,
            &format!(
                "INSERT INTO places {{id:type::record('places',{}),location:geo::point({point})}}",
                360 + i
            ),
        );
    }
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    q(&c, "COMMIT");
    for point in [
        "0,0",
        "180,0",
        "-180,0",
        "45,90",
        "70,-90",
        "17,10",
        "179.99999,89.99999",
    ] {
        let center = q(&c, &format!("SELECT geo::point({point})")).rows[0][0].clone();
        for radius in [
            0.0,
            1.0,
            100_000.0,
            2_000_000.0,
            20_015_114.0,
            21_000_000.0,
            f64::MAX,
        ] {
            let params = Parameters::from([
                ("$point".into(), center.clone()),
                ("$radius".into(), Value::Number(radius)),
            ]);
            let scan=c.execute("SELECT id,geo::distance(location,$point) AS distance_m FROM places NOT INDEXED WHERE geo::within(location,$point,$radius) ORDER BY distance_m,id",&params).unwrap();
            assert_eq!(
                near(&c, &center, radius),
                scan.rows,
                "center {point}, radius {radius}"
            );
        }
        // Exact floating-point boundaries must use identical refinement semantics.
        for row in q(
            &c,
            &format!("SELECT geo::distance(location,geo::point({point})) FROM places LIMIT 6"),
        )
        .rows
        {
            let Value::Number(radius) = row[0] else {
                panic!()
            };
            let params = Parameters::from([
                ("$point".into(), center.clone()),
                ("$radius".into(), Value::Number(radius)),
            ]);
            let scan=c.execute("SELECT id,geo::distance(location,$point) AS distance_m FROM places NOT INDEXED WHERE geo::within(location,$point,$radius) ORDER BY distance_m,id",&params).unwrap();
            assert_eq!(
                near(&c, &center, radius),
                scan.rows,
                "boundary center {point}"
            );
        }
    }
    let sql =
        "SELECT * FROM search::near('places_location',geo::point(0,0),100) ORDER BY distance_m,id";
    let plan = q(&c, &format!("EXPLAIN QUERY PLAN {sql}"));
    assert!(format!("{:?}", plan.rows).contains("places_location"));
    let profile = c.profile_select(sql, &Parameters::new()).unwrap();
    assert!(profile.metrics.rows_read < 40, "{:?}", profile.metrics);
    assert_eq!(profile.metrics.fullscan_steps, 0, "{:?}", profile.metrics);
    println!("spatial range profile: {:?}", profile.metrics);
    // Filtering, explicit ties/pagination and joins use the ordinary SELECT pipeline.
    assert_eq!(q(&c,"SELECT p.id,n.distance_m FROM search::near('places_location',geo::point(0,0),100) n JOIN places p ON n.id=p.id ORDER BY n.distance_m,p.id LIMIT 1").rows.len(),1);
    assert_eq!(q(&c,"WITH hits AS (SELECT * FROM search::near('places_location',geo::point(0,0),100)) SELECT * FROM hits ORDER BY distance_m,id").rows,profile.result.rows);
}

#[test]
fn failed_spatial_build_and_invalid_search_leave_prior_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "INSERT INTO places {id:places:a,location:geo::point(0,0)}",
    );
    q(&c, "INSERT INTO places {id:places:b,location:'invalid'}");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO notes {id:notes:n,n:1}");
    assert!(c
        .execute(
            "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
            &Parameters::new()
        )
        .is_err());
    q(&c, "UPDATE places:b {location:geo::point(1,1)}");
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    for sql in [
        "SELECT * FROM search::near('missing',geo::point(0,0),100)",
        "SELECT * FROM search::near('places_location',geo::point(0,0),-1)",
        "SELECT * FROM search::near('places_location',geo::point(0,0),random())",
        "SELECT * FROM search::near('places_location',geo::point(0,0))",
        "SELECT * FROM search::near('places_location',NULL,100)",
        "SELECT * FROM search::near('places_location',$missing,100)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(q(&c, "SELECT * FROM notes").rows.len(), 1);
    c.check_collection_integrity("places", Default::default())
        .unwrap();
    q(&c, "ROLLBACK");
    assert!(c
        .execute(
            "SELECT * FROM search::near('places_location',geo::point(0,0),100)",
            &Parameters::new()
        )
        .is_err());
}

#[test]
fn spatial_search_retains_exact_boundary_near_opposite_poles() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(
        &c,
        "INSERT INTO places {id:places:a,location:geo::point(0,-89.9999964108325)}",
    );
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    let center = q(&c, "SELECT geo::point(0,89.99999479428419)").rows[0][0].clone();
    let radius = c
        .execute(
            "SELECT geo::distance(location,$point) FROM places",
            &Parameters::from([("$point".into(), center.clone())]),
        )
        .unwrap()
        .rows[0][0]
        .clone();
    let Value::Number(radius) = radius else {
        panic!()
    };
    assert_eq!(near(&c, &center, radius).len(), 1);
}

#[test]
fn h3_cells_match_reference_and_canonical_geographic_positions() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    // Reference example from H3's official latLngToCell documentation.
    assert_eq!(
        q(&c, "SELECT geo::cell(geo::point(40,45),2)").rows,
        vec![vec![Value::String("822d57fffffffff".into())]]
    );
    for resolution in 0..=15 {
        for (a, b) in [
            ("180,0", "-180,0"),
            ("180,90", "-73,90"),
            ("-180,-90", "97,-90"),
        ] {
            assert_eq!(
                q(
                    &c,
                    &format!("SELECT geo::cell(geo::point({a}),{resolution})")
                )
                .rows,
                q(
                    &c,
                    &format!("SELECT geo::cell(geo::point({b}),{resolution})")
                )
                .rows
            );
        }
        // Center-to-cell identity covers different resolutions, dateline and poles.
        for point in [
            "40,45",
            "0,0",
            "180,0",
            "17,90",
            "-120,-90",
            "-122.41795063018799,37.775938728915946",
        ] {
            let value = q(
                &c,
                &format!("SELECT geo::cell(geo::point({point}),{resolution})"),
            )
            .rows[0][0]
                .clone();
            let params = Parameters::from([("$cell".into(), value.clone())]);
            assert_eq!(
                c.execute(
                    &format!("SELECT geo::cell(geo::cell_center($cell),{resolution})"),
                    &params
                )
                .unwrap()
                .rows,
                vec![vec![value]]
            );
        }
    }
    for sql in [
        "SELECT geo::cell(geo::point(0,0),-1)",
        "SELECT geo::cell(geo::point(0,0),16)",
        "SELECT geo::cell(geo::point(0,0),2.5)",
        "SELECT geo::cell(geo::point(0,0),'2')",
        "SELECT geo::cell(geo::point(0,0),NULL)",
        "SELECT geo::cell(NULL,2)",
        "SELECT geo::cell_center('0')",
        "SELECT geo::cell_center('822D57FFFFFFFFF')",
        "SELECT geo::cell_center('0822d57fffffffff')",
        "SELECT geo::cell_center('122d57fffffffff')",
        "SELECT geo::cell_center(NULL)",
        "SELECT geo::cell_center(2)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn h3_cell_groups_persist_and_preserve_failed_write_atomicity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cells.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c,"INSERT INTO places {id:places:a,location:geo::point(40,45),cell:geo::cell(geo::point(40,45),2)}");
        q(&c,"INSERT INTO places {id:places:b,location:geo::point(40,45),cell:geo::cell(geo::point(40,45),2)}");
        q(&c, "CREATE INDEX place_cells ON places(cell)");
        q(&c, "BEGIN");
        assert!(c
            .execute(
                "UPDATE places SET cell=geo::cell(location,20)",
                &Parameters::new()
            )
            .is_err());
        q(&c, "UPDATE places SET cell=geo::cell(location,3)");
        q(&c, "ROLLBACK");
        c.check_collection_integrity("places", Default::default())
            .unwrap();
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let expected = vec![vec![
        Value::String("822d57fffffffff".into()),
        Value::Integer(2),
    ]];
    assert_eq!(q(&c,"SELECT geo::cell(location,2) AS cell,count(*) AS n FROM places GROUP BY geo::cell(location,2) HAVING count(*)>1 ORDER BY cell").rows,expected);
    assert_eq!(
        q(
            &c,
            "SELECT cell,count(*) FROM places GROUP BY cell ORDER BY cell"
        )
        .rows,
        expected
    );
    assert_eq!(
        c.lookup_index(
            "places",
            "place_cells",
            &Value::String("822d57fffffffff".into())
        )
        .unwrap()
        .len(),
        2
    );
    q(&c, "CREATE TABLE cells");
    q(&c,"INSERT INTO cells(cell,n) SELECT geo::cell(location,2),count(*) FROM places GROUP BY geo::cell(location,2)");
    assert_eq!(q(&c, "SELECT cell,n FROM cells").rows, expected);
}

fn one(c: &romedb::Connection, sql: &str, params: &Parameters) -> Value {
    let result = c.execute(sql, params).expect(sql);
    assert_eq!(result.rows.len(), 1, "{sql}");
    result.rows[0][0].clone()
}

const STORM: &str = r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[4,4],[4,6],[6,6],[6,4],[4,4]]],"properties":{"name":"storm"}}"#;

#[test]
fn region_predicates_match_in_native_and_postgis_spellings() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let params = Parameters::from([("$storm".into(), Value::String(STORM.into()))]);
    let cases = [
        ((1.0, 1.0), true, true),
        ((0.0, 5.0), false, true),
        ((5.0, 5.0), false, false),
        ((4.0, 5.0), false, true),
        ((11.0, 5.0), false, false),
    ];
    for ((x, y), contains, covers) in cases {
        let point = format!("geo::point({x},{y})");
        let st_point = format!("ST_Point({x},{y})");
        for (sql, expected) in [
            (
                format!("SELECT geo::contains(geo::from_geojson($storm),{point})"),
                contains,
            ),
            (
                format!("SELECT ST_Contains(ST_GeomFromGeoJSON($storm),{st_point})"),
                contains,
            ),
            (
                format!("SELECT st_within({st_point},st_geomfromgeojson($storm))"),
                contains,
            ),
            (
                format!("SELECT geo::inside({point},geo::from_geojson($storm))"),
                contains,
            ),
            (
                format!("SELECT geo::covers(geo::from_geojson($storm),{point})"),
                covers,
            ),
            (
                format!("SELECT ST_Covers(ST_GeomFromGeoJSON($storm),{st_point})"),
                covers,
            ),
            (
                format!("SELECT ST_Intersects({st_point},ST_GeomFromGeoJSON($storm))"),
                covers,
            ),
            (
                format!("SELECT geo::intersects(geo::from_geojson($storm),{point})"),
                covers,
            ),
        ] {
            assert_eq!(one(&c, &sql, &params), Value::Boolean(expected), "{sql}");
        }
    }
}

#[test]
fn constructors_accessors_and_geojson_round_trip_through_sql() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let none = Parameters::new();
    assert_eq!(
        one(&c, "SELECT ST_Point(106.7,10.8)", &none),
        one(&c, "SELECT geo::point(106.7,10.8)", &none)
    );
    assert_eq!(
        one(&c, "SELECT ST_MakePoint(106.7,10.8)", &none),
        one(&c, "SELECT ST_Point(106.7,10.8,4326)", &none)
    );
    assert_eq!(
        one(&c, "SELECT ST_X(ST_Point(106.7,10.8))", &none),
        Value::Number(106.7)
    );
    assert_eq!(
        one(&c, "SELECT geo::lat(geo::point(106.7,10.8))", &none),
        Value::Number(10.8)
    );
    assert_eq!(
        one(&c, "SELECT ST_AsGeoJSON(ST_Point(106.7,-0.0))", &none),
        Value::String(r#"{"type":"Point","coordinates":[106.7,-0.0]}"#.into())
    );
    let params = Parameters::from([("$storm".into(), Value::String(STORM.into()))]);
    let Value::String(text) = one(
        &c,
        "SELECT geo::to_geojson(geo::from_geojson($storm))",
        &params,
    ) else {
        panic!()
    };
    assert!(
        text.starts_with(r#"{"type":"Polygon","coordinates":[[[0.0,0.0],[10.0,0.0]"#),
        "{text}"
    );
    assert!(!text.contains("properties"));
    let round = Parameters::from([("$storm".into(), Value::String(text.clone()))]);
    assert_eq!(
        one(
            &c,
            "SELECT ST_AsGeoJSON(ST_GeomFromGeoJSON($storm))",
            &round
        ),
        Value::String(text)
    );
    let envelope = one(&c, "SELECT ST_MakeEnvelope(100,10,110,20,4326)", &none);
    assert_eq!(
        envelope,
        one(&c, "SELECT geo::envelope(100,10,110,20)", &none)
    );
    assert_eq!(
        one(
            &c,
            "SELECT ST_Covers(ST_MakeEnvelope(100,10,110,20),ST_Point(110,20))",
            &none
        ),
        Value::Boolean(true)
    );
    let a = "ST_Point(0,0)";
    let b = "ST_Point(1,0)";
    assert_eq!(
        one(&c, &format!("SELECT ST_DistanceSphere({a},{b})"), &none),
        one(&c, &format!("SELECT geo::distance({a},{b})"), &none)
    );
    let Value::Number(meters) = one(&c, &format!("SELECT ST_DistanceSphere({a},{b})"), &none)
    else {
        panic!()
    };
    for (radius, expected) in [(meters, true), (meters - 1e-6, false)] {
        assert_eq!(
            one(&c, &format!("SELECT ST_DWithin({a},{b},{radius:?})"), &none),
            Value::Boolean(expected)
        );
    }
    assert_eq!(
        one(
            &c,
            &format!("SELECT ST_DWithin({a},{b},{meters:?},false)"),
            &none
        ),
        Value::Boolean(true)
    );
    let named = c
        .execute(
            "SELECT ST_X(geo::point(1,2)), geo::lng(geo::point(1,2))",
            &none,
        )
        .unwrap();
    assert!(named.columns[0].contains("ST_X"), "{:?}", named.columns);
    assert!(named.columns[1].contains("geo::lng"), "{:?}", named.columns);
}

#[test]
fn new_functions_propagate_null_while_legacy_functions_stay_strict() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let none = Parameters::new();
    for expr in [
        "ST_Point(NULL,1)",
        "ST_X(NULL)",
        "ST_DistanceSphere(NULL,ST_Point(0,0))",
        "ST_DWithin(ST_Point(0,0),ST_Point(0,0),NULL)",
        "geo::contains(NULL,geo::point(0,0))",
        "geo::covers(geo::envelope(0,0,1,1),NULL)",
        "ST_GeomFromGeoJSON(NULL)",
        "geo::cell_parent(NULL,3)",
        "geo::cell_disk(geo::cell(geo::point(0,0),5),NULL)",
    ] {
        assert_eq!(
            one(&c, &format!("SELECT {expr}"), &none),
            Value::Null,
            "{expr}"
        );
    }
    for expr in [
        "geo::point(NULL,1)",
        "geo::distance(NULL,geo::point(0,0))",
        "geo::within(geo::point(0,0),geo::point(0,0),NULL)",
        "geo::cell(NULL,3)",
    ] {
        assert!(
            c.execute(&format!("SELECT {expr}"), &none).is_err(),
            "{expr}"
        );
    }
}

#[test]
fn invalid_regions_and_pairs_report_validation_and_limit_errors() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let none = Parameters::new();
    let error = |sql: &str, params: &Parameters| c.execute(sql, params).unwrap_err().code();
    for (text, code) in [
        (
            r#"{"type":"Polygon","coordinates":[[[0,0],[2,2],[2,0],[0,2],[0,0]]]}"#,
            "FDB_VALIDATION",
        ),
        (r#"{"type":"Feature","geometry":null}"#, "FDB_VALIDATION"),
        (
            r#"{"type":"Point","coordinates":[1,2],"crs":null}"#,
            "FDB_VALIDATION",
        ),
        (
            r#"{"type":"Point","coordinates":[1,2],"type":"Point"}"#,
            "FDB_VALIDATION",
        ),
        ("not json", "FDB_VALIDATION"),
    ] {
        let params = Parameters::from([("$g".into(), Value::String(text.into()))]);
        assert_eq!(
            error("SELECT ST_GeomFromGeoJSON($g)", &params),
            code,
            "{text}"
        );
    }
    let huge = format!("[{}]", "0,".repeat(600_000));
    let params = Parameters::from([("$g".into(), Value::String(huge))]);
    assert_eq!(error("SELECT ST_GeomFromGeoJSON($g)", &params), "FDB_LIMIT");
    for sql in [
        "SELECT ST_Contains(ST_Point(0,0),ST_Point(0,0))",
        "SELECT ST_Intersects(ST_Point(0,0),ST_Point(0,0))",
        "SELECT ST_Contains(ST_MakeEnvelope(0,0,1,1),ST_MakeEnvelope(0,0,1,1))",
        "SELECT ST_MakeEnvelope(170,0,-170,1)",
        "SELECT ST_MakeEnvelope(-100,0,100,1)",
        "SELECT ST_Point(1,2,3857)",
        "SELECT ST_DWithin(ST_Point(0,0),ST_Point(0,0),1,true)",
        "SELECT geo::cell_boundary('not a cell')",
    ] {
        assert_eq!(error(sql, &none), "FDB_VALIDATION", "{sql}");
    }
    assert_eq!(
        error(
            "SELECT geo::cell_disk(geo::cell(geo::point(0,0),9),200)",
            &none
        ),
        "FDB_LIMIT"
    );
}

#[test]
fn stored_regions_and_points_work_in_writes_and_filters() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("regions.db");
    let params = Parameters::from([("$storm".into(), Value::String(STORM.into()))]);
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        c.execute(
            "INSERT INTO zones {id:zones:storm,shape:geo::from_geojson($storm)}",
            &params,
        )
        .unwrap();
        q(
            &c,
            "INSERT INTO zones {id:zones:box,shape:ST_MakeEnvelope(20,20,30,30)}",
        );
        let mut expected = Vec::new();
        for x in -1..=12 {
            for y in -1..=12 {
                q(
                    &c,
                    &format!(
                        "INSERT INTO houses {{id:houses:h_{}_{},location:ST_Point({x},{y})}}",
                        x + 1,
                        y + 1
                    ),
                );
                let inside_outer = (0..=10).contains(&x) && (0..=10).contains(&y);
                let inside_hole = (4..6).contains(&x) && (4..6).contains(&y) && x > 4 && y > 4;
                if inside_outer && !inside_hole {
                    expected.push(format!("h_{}_{}", x + 1, y + 1));
                }
            }
        }
        q(
            &c,
            "INSERT INTO houses {id:houses:missing,name:'no location'}",
        );
        expected.sort();
        let rows = c
            .execute(
                "SELECT record::id(id) FROM houses WHERE ST_Covers(ST_GeomFromGeoJSON($storm),location) ORDER BY id",
                &params,
            )
            .unwrap()
            .rows;
        let mut found = rows
            .into_iter()
            .map(|row| match &row[0] {
                Value::String(s) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>();
        found.sort();
        assert_eq!(found, expected);
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let zones = c
        .execute(
            "SELECT record::id(id) FROM zones WHERE geo::covers(shape,geo::point(25,25)) ORDER BY id",
            &Parameters::new(),
        )
        .unwrap()
        .rows;
    assert_eq!(zones, vec![vec![Value::String("box".into())]]);
    assert_eq!(
        one(
            &c,
            "SELECT ST_Contains(shape,ST_Point(5,5)) FROM zones WHERE id=zones:storm",
            &Parameters::new()
        ),
        Value::Boolean(false)
    );
    q(
        &c,
        "INSERT INTO houses {id:houses:nullable,location:ST_Point(NULL,1)}",
    );
    assert_eq!(
        one(
            &c,
            "SELECT location FROM houses WHERE id=houses:nullable",
            &Parameters::new()
        ),
        Value::Null
    );
}

#[test]
fn h3_hierarchy_neighborhood_and_boundary_functions_compose() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let none = Parameters::new();
    let p = "geo::point(106.7,10.8)";
    assert_eq!(
        one(
            &c,
            &format!("SELECT geo::cell_parent(geo::cell({p},9),7)"),
            &none
        ),
        one(&c, &format!("SELECT geo::cell({p},7)"), &none)
    );
    assert_eq!(
        one(
            &c,
            &format!("SELECT geo::cell_resolution(geo::cell({p},9))"),
            &none
        ),
        Value::Integer(9)
    );
    assert_eq!(
        one(
            &c,
            &format!("SELECT array::len(geo::cell_children(geo::cell({p},7),9))"),
            &none
        ),
        Value::Integer(49)
    );
    assert_eq!(
        one(
            &c,
            &format!("SELECT array::len(geo::cell_disk(geo::cell({p},9),1))"),
            &none
        ),
        Value::Integer(7)
    );
    assert_eq!(
        one(
            &c,
            &format!("SELECT geo::cell_compact(geo::cell_children(geo::cell({p},7),9))"),
            &none
        ),
        Value::Array(vec![one(&c, &format!("SELECT geo::cell({p},7)"), &none)])
    );
    for resolution in [0, 3, 7, 12, 15] {
        let cell = format!("geo::cell({p},{resolution})");
        assert_eq!(
            one(
                &c,
                &format!("SELECT geo::covers(geo::cell_boundary({cell}),geo::cell_center({cell}))"),
                &none
            ),
            Value::Boolean(true),
            "{resolution}"
        );
    }
    let Value::Number(area) = one(
        &c,
        &format!("SELECT geo::cell_area(geo::cell({p},9))"),
        &none,
    ) else {
        panic!()
    };
    assert!((50_000.0..250_000.0).contains(&area), "{area}");
}

#[test]
fn radius_search_accepts_postgis_point_constructors() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE places");
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    q(
        &c,
        "INSERT INTO places {id:places:a,location:ST_Point(106.7,10.8)}",
    );
    let native = q(
        &c,
        "SELECT id FROM search::near('places_location',geo::point(106.7,10.8),10)",
    )
    .rows;
    let alias = q(
        &c,
        "SELECT id FROM search::near('places_location',ST_Point(106.7,10.8),10)",
    )
    .rows;
    assert_eq!(native.len(), 1);
    assert_eq!(native, alias);
}

fn reference_bbox(p: (f64, f64), w: f64, s: f64, e: f64, n: f64) -> bool {
    let inside = |x: f64| {
        if w <= e {
            w <= x && x <= e
        } else {
            x >= w || x <= e
        }
    };
    s <= p.1 && p.1 <= n && (inside(p.0) || (p.0.abs() == 180.0 && inside(-p.0)))
}

fn ids(rows: Vec<Vec<Value>>) -> Vec<String> {
    let mut ids = rows
        .into_iter()
        .map(|row| format!("{:?}", row[0]))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn indexed_places(c: &romedb::Connection) -> Vec<(String, (f64, f64))> {
    q(c, "CREATE TABLE places");
    q(c, "BEGIN");
    for i in 0..2000u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 18001) as f64 / 100.0 - 90.0;
        q(c, &format!("INSERT INTO places {{id:type::record('places',{i}),location:geo::point({longitude},{latitude})}}"));
    }
    for (i, point) in [
        "180,5", "-180,-5", "180,0", "0,90", "10,-90", "10,0", "10,10", "20,10",
    ]
    .iter()
    .enumerate()
    {
        q(
            c,
            &format!(
                "INSERT INTO places {{id:type::record('places',{}),location:geo::point({point})}}",
                3000 + i
            ),
        );
    }
    q(c, "INSERT INTO places {id:places:missing}");
    q(c, "INSERT INTO places {id:places:nothing,location:NULL}");
    q(
        c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    q(c, "COMMIT");
    q(
        c,
        "SELECT id, geo::lng(location), geo::lat(location) FROM places",
    )
    .rows
    .into_iter()
    .filter_map(|row| match (&row[1], &row[2]) {
        (Value::Number(x), Value::Number(y)) => Some((format!("{:?}", row[0]), (*x, *y))),
        (Value::Null, Value::Null) => None,
        _ => panic!("{row:?}"),
    })
    .collect()
}

#[test]
fn viewport_source_matches_reference_and_uses_the_index() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let points = indexed_places(&c);
    for (w, s, e, n) in [
        (0.0, 0.0, 10.0, 10.0),
        (-30.5, -12.25, 45.0, 60.0),
        (170.0, -10.0, -170.0, 10.0),
        (10.0, -90.0, 10.0, 90.0),
        (-180.0, 10.0, 180.0, 10.0),
        (-180.0, -90.0, 180.0, 90.0),
        (180.0, -5.0, 180.0, 5.0),
        (-180.0, -5.0, -180.0, 5.0),
        (5.0, 89.0, 15.0, 90.0),
        (179.99, -90.0, -179.99, 90.0),
    ] {
        let params = Parameters::from([
            ("$w".into(), Value::Number(w)),
            ("$s".into(), Value::Number(s)),
            ("$e".into(), Value::Number(e)),
            ("$n".into(), Value::Number(n)),
        ]);
        let found = ids(c
            .execute(
                "SELECT id FROM search::bbox('places_location',$w,$s,$e,$n)",
                &params,
            )
            .unwrap()
            .rows);
        let mut expected = points
            .iter()
            .filter(|(_, p)| reference_bbox(*p, w, s, e, n))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(found, expected, "viewport {w},{s},{e},{n}");
    }
    let sql = "SELECT id FROM search::bbox('places_location',0,0,1,1)";
    let profile = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(profile.metrics.fullscan_steps, 0, "{:?}", profile.metrics);
    assert!(profile.metrics.rows_read < 40, "{:?}", profile.metrics);
    let joined = q(&c, "SELECT p.id FROM search::bbox('places_location',5,5,25,15) AS v JOIN places AS p ON p.id=v.id ORDER BY p.id");
    assert!(joined.rows.len() >= 2);
    for sql in [
        "SELECT id FROM search::bbox('places_location',0,10,1,0)",
        "SELECT id FROM search::bbox('places_location',0,0,181,1)",
        "SELECT id FROM search::bbox('places_location',0,0,1)",
        "SELECT id FROM search::bbox('missing_index',0,0,1,1)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
}

#[test]
fn polygon_source_matches_scalar_predicates_and_uses_the_index() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    indexed_places(&c);
    let regions = [
        STORM.to_owned(),
        r#"{"type":"MultiPolygon","coordinates":[[[[170,-10],[180,-10],[180,10],[170,10],[170,-10]]],[[[-180,-10],[-170,-10],[-170,10],[-180,10],[-180,-10]]]]}"#.to_owned(),
        r#"{"type":"Polygon","coordinates":[[[-50,-40],[-49.9,-40],[60,50],[59.9,50],[-50,-40]]]}"#.to_owned(),
        r#"{"type":"Polygon","coordinates":[[[10,0],[20,10],[10,10],[10,0]]]}"#.to_owned(),
    ];
    for region in &regions {
        let params = Parameters::from([("$r".into(), Value::String(region.clone()))]);
        for (mode, predicate) in [("contains", "ST_Contains"), ("covers", "ST_Covers")] {
            let found = ids(c
                .execute(
                    &format!("SELECT id FROM search::polygon('places_location',ST_GeomFromGeoJSON($r),'{mode}')"),
                    &params,
                )
                .unwrap()
                .rows);
            let expected = ids(c
                .execute(
                    &format!("SELECT id FROM places NOT INDEXED WHERE {predicate}(ST_GeomFromGeoJSON($r),location)"),
                    &params,
                )
                .unwrap()
                .rows);
            assert_eq!(found, expected, "{mode} {region}");
        }
    }
    let covers = ids(q(
        &c,
        "SELECT id FROM search::polygon('places_location',geo::envelope(10,0,20,10),'covers')",
    )
    .rows);
    let contains = ids(q(
        &c,
        "SELECT id FROM search::polygon('places_location',ST_MakeEnvelope(10,0,20,10),'contains')",
    )
    .rows);
    for corner in [3005, 3006, 3007] {
        let id = format!(
            "{:?}",
            Value::Record(romedb::Record {
                table: "places".into(),
                key: romedb::Key::Integer(corner)
            })
        );
        assert!(covers.contains(&id) && !contains.contains(&id), "{id}");
    }
    assert!(contains.iter().all(|id| covers.contains(id)));
    assert_eq!(covers.len(), contains.len() + 3);
    let sql = "SELECT id FROM search::polygon('places_location',geo::envelope(0,0,1,1),'covers')";
    let profile = c.profile_select(sql, &Parameters::new()).unwrap();
    assert_eq!(profile.metrics.fullscan_steps, 0, "{:?}", profile.metrics);
    assert!(profile.metrics.rows_read < 40, "{:?}", profile.metrics);
    for sql in [
        "SELECT id FROM search::polygon('places_location',geo::envelope(0,0,1,1),'within')",
        "SELECT id FROM search::polygon('places_location',geo::point(0,0),'covers')",
        "SELECT id FROM search::polygon('places_location',geo::envelope(0,0,1,1))",
    ] {
        assert_eq!(
            c.execute(sql, &Parameters::new()).unwrap_err().code(),
            "FDB_VALIDATION",
            "{sql}"
        );
    }
}

#[test]
fn nearest_source_matches_exhaustive_order_with_typed_ties() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    indexed_places(&c);
    for id in [
        "type::record('places','tie_b')",
        "type::record('places','tie_a')",
        "places:999999",
        "type::record('places',-5)",
        "type::record('places','tie_c')",
    ] {
        q(
            &c,
            &format!("INSERT INTO places {{id:{id},location:geo::point(50.5,50.5)}}"),
        );
    }
    for (x, y) in [
        (0.0, 0.0),
        (180.0, 0.0),
        (-180.0, 1.0),
        (10.0, 90.0),
        (-60.0, -89.0),
        (50.5, 50.5),
        (123.456, -45.5),
    ] {
        for k in [1, 3, 5, 50, 3000] {
            let params = Parameters::from([
                (
                    "$c".into(),
                    Value::Object(romedb::Document::from([
                        ("type".into(), Value::String("Point".into())),
                        (
                            "coordinates".into(),
                            Value::Array(vec![Value::Number(x), Value::Number(y)]),
                        ),
                    ])),
                ),
                ("$k".into(), Value::Integer(k)),
            ]);
            let found = c
                .execute("SELECT id, distance_m FROM search::nearest('places_location',$c,$k) ORDER BY distance_m, id", &params)
                .unwrap()
                .rows;
            let expected = c
                .execute(
                    "SELECT id, ST_DistanceSphere(location,$c) AS d FROM places NOT INDEXED WHERE ST_DistanceSphere(location,$c) IS NOT NULL ORDER BY d, id LIMIT $k",
                    &params,
                )
                .unwrap()
                .rows;
            assert_eq!(found, expected, "center {x},{y} k {k}");
            assert_eq!(found.len() as i64, k.min(2013), "center {x},{y} k {k}");
        }
    }
    let tie = q(&c, "SELECT id FROM search::nearest('places_location',geo::point(50.5,50.5),3) ORDER BY distance_m, id").rows;
    let typed = q(
        &c,
        "SELECT id FROM places WHERE geo::lng(location)=50.5 ORDER BY id LIMIT 3",
    )
    .rows;
    assert_eq!(tie, typed);
    let allowed = q(&c, "SELECT id FROM search::nearest('places_location',geo::point(0,0),5,array::new(places:3001,places:3002,places:7)) ORDER BY distance_m, id").rows;
    assert_eq!(allowed.len(), 3);
    let joined = q(&c, "SELECT p.id, n.distance_m FROM search::nearest('places_location',ST_Point(10,10),5) AS n JOIN places AS p ON p.id=n.id ORDER BY n.distance_m, p.id");
    assert_eq!(joined.rows.len(), 5);
    assert_eq!(joined.rows[0][1], Value::Number(0.0));
    for sql in [
        "SELECT id FROM search::nearest('places_location',geo::point(0,0),0)",
        "SELECT id FROM search::nearest('places_location',geo::point(0,0),10001)",
        "SELECT id FROM search::nearest('places_location',geo::point(0,0),1.5)",
        "SELECT id FROM search::nearest('places_location',geo::point(0,0))",
        "SELECT id FROM search::nearest('places_location',geo::envelope(0,0,1,1),3)",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    let profile = c
        .profile_select(
            "SELECT id FROM search::nearest('places_location',geo::point(0,0),5)",
            &Parameters::new(),
        )
        .unwrap();
    assert!(profile.metrics.spatial_rounds >= 1, "{:?}", profile.metrics);
    assert!(
        profile.metrics.spatial_ranges >= profile.metrics.spatial_rounds,
        "{:?}",
        profile.metrics
    );
    let profile = c
        .profile_select(
            "SELECT id FROM search::bbox('places_location',0,0,1,1)",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(profile.metrics.spatial_rounds, 0, "{:?}", profile.metrics);
    assert!(
        (1..=8).contains(&profile.metrics.spatial_ranges),
        "{:?}",
        profile.metrics
    );
    q(&c, "BEGIN");
    q(
        &c,
        "INSERT INTO places {id:places:pending,location:geo::point(0.0001,0.0001)}",
    );
    let pending = q(
        &c,
        "SELECT id FROM search::nearest('places_location',geo::point(0.0001,0.0001),1)",
    )
    .rows;
    assert_eq!(
        pending[0][0],
        Value::Record(romedb::Record {
            table: "places".into(),
            key: romedb::Key::String("pending".into())
        })
    );
    q(&c, "ROLLBACK");
    let after = q(
        &c,
        "SELECT id FROM search::nearest('places_location',geo::point(0.0001,0.0001),1)",
    )
    .rows;
    assert_ne!(after[0][0], pending[0][0]);
}

fn drivers(c: &romedb::Connection) {
    q(c, "CREATE TABLE drivers");
    q(c, "BEGIN");
    for i in 0..3000u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 18001) as f64 / 100.0 - 90.0;
        q(c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{i}),available:{},last_seen:{},location:geo::point({longitude},{latitude})}}",
            i % 7 == 0,
            i % 100
        ));
    }
    for i in 0..40u64 {
        q(c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{}),available:true,last_seen:99,location:geo::point({},{})}}",
            5000 + i,
            106.7 + (i % 5) as f64 * 0.001,
            10.8 + (i / 5) as f64 * 0.001
        ));
    }
    q(
        c,
        "INSERT INTO drivers {id:drivers:missing,available:true,last_seen:99}",
    );
    q(
        c,
        "INSERT INTO drivers {id:drivers:nothing,available:true,last_seen:99,location:NULL}",
    );
    q(
        c,
        "CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL",
    );
    q(c, "COMMIT");
}

fn same_as_scan(c: &romedb::Connection, sql: &str, all: &Parameters) -> romedb::ProfiledQuery {
    let params = &all
        .iter()
        .filter(|(name, _)| sql.contains(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Parameters>();
    let profile = c.profile_select(sql, params).expect(sql);
    let scan = if sql.contains("FROM drivers AS d") {
        sql.replacen("FROM drivers AS d", "FROM drivers AS d NOT INDEXED", 1)
    } else {
        sql.replacen("FROM drivers", "FROM drivers NOT INDEXED", 1)
    };
    assert_ne!(scan, sql);
    let expected = c.execute(&scan, params).expect(&scan).rows;
    assert_eq!(profile.result.rows, expected, "{sql}");
    profile
}

#[test]
fn ordinary_where_forms_use_the_index_without_changing_results() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    drivers(&c);
    let params = Parameters::from([
        ("$storm".into(), Value::String(STORM.into())),
        ("$x".into(), Value::Number(106.7)),
        ("$y".into(), Value::Number(10.8)),
        ("$r".into(), Value::Integer(500)),
        ("$fresh".into(), Value::Integer(50)),
    ]);
    for sql in [
        "SELECT id FROM drivers WHERE ST_Covers(ST_GeomFromGeoJSON($storm), location) ORDER BY id",
        "SELECT id FROM drivers WHERE ST_Contains(geo::envelope(100, 0, 140, 40), location) AND available = true ORDER BY id",
        "SELECT d.id FROM drivers AS d WHERE ST_Within(d.location, ST_GeomFromGeoJSON($storm)) ORDER BY d.id",
        "SELECT id FROM drivers WHERE ST_Intersects(location, geo::envelope(100, 0, 110, 20)) AND last_seen >= $fresh ORDER BY id",
        "SELECT id FROM drivers WHERE geo::intersects(geo::envelope(100, 0, 110, 20), location) ORDER BY id",
        "SELECT id FROM drivers WHERE (geo::covers(geo::envelope(100, 0, 110, 20), location)) ORDER BY id",
        "SELECT id, ST_DistanceSphere(location, ST_Point($x, $y)) AS d FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) <= $r ORDER BY d, id",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(ST_Point($x, $y), location) < 300 AND available = true ORDER BY id",
        "SELECT id FROM drivers WHERE 300 >= ST_DistanceSphere(location, ST_Point($x, $y)) ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point($x, $y), 400) ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DWithin(ST_Point($x, $y), location, 400, false) ORDER BY id",
    ] {
        let profile = same_as_scan(&c, sql, &params);
        assert!(!profile.result.rows.is_empty(), "{sql}");
        assert_eq!(profile.metrics.fullscan_steps, 0, "{sql}: {:?}", profile.metrics);
        let used = params
            .iter()
            .filter(|(name, _)| sql.contains(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Parameters>();
        let plan = format!("{:?}", c.execute(&format!("EXPLAIN QUERY PLAN {sql}"), &used).unwrap().rows);
        assert!(plan.contains("drivers_location"), "{sql}: {plan}");
    }
    for sql in [
        "SELECT id FROM drivers WHERE ST_Covers(NULL, location) ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, NULL) <= 10 ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point(0, 0), NULL) ORDER BY id",
    ] {
        let profile = same_as_scan(&c, sql, &params);
        assert!(profile.result.rows.is_empty(), "{sql}");
    }
    for sql in [
        "SELECT id, ST_Point(106.7, 10.8) AS location FROM drivers WHERE ST_DWithin(location, ST_Point(106.7, 10.8), 10) ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(0, 0)) <= -1 ORDER BY id",
        "SELECT id FROM drivers WHERE ST_Covers(ST_GeomFromGeoJSON($storm), location) OR available = true ORDER BY id",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(0, 0)) <= '500' ORDER BY id",
    ] {
        same_as_scan(&c, sql, &params);
    }
    for sql in [
        "SELECT id FROM drivers WHERE geo::within(location, geo::point(0, 0), 100)",
        "SELECT id FROM drivers WHERE geo::distance(location, geo::point(0, 0)) <= 100",
        "SELECT id FROM drivers WHERE ST_Covers(ST_Point(0, 0), location)",
        "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point(0, 0), -1)",
    ] {
        let none = Parameters::new();
        assert!(c.execute(sql, &none).is_err(), "{sql}");
        assert!(
            c.execute(
                &sql.replacen("FROM drivers", "FROM drivers NOT INDEXED", 1),
                &none
            )
            .is_err(),
            "{sql}"
        );
    }
}

#[test]
fn ordinary_nearest_form_matches_scan_order_with_filters() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    drivers(&c);
    for (x, y) in [(106.7, 10.8), (0.0, 0.0), (179.9, -0.5), (-60.0, 85.0)] {
        for k in [1, 5, 50] {
            let params = Parameters::from([
                ("$x".into(), Value::Number(x)),
                ("$y".into(), Value::Number(y)),
                ("$k".into(), Value::Integer(k)),
                ("$fresh".into(), Value::Integer(90)),
            ]);
            for sql in [
                "SELECT id, ST_DistanceSphere(location, ST_Point($x, $y)) AS distance_m FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND available = true AND last_seen >= $fresh ORDER BY distance_m, id LIMIT $k",
                "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)) LIMIT $k",
                "SELECT d.id FROM drivers AS d WHERE ST_DistanceSphere(ST_Point($x, $y), d.location) IS NOT NULL AND d.available = true ORDER BY geo::distance(d.location, geo::point($x, $y)), d.id LIMIT $k",
                "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point($x, $y), 3000000) AND available = false ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT $k",
                "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND last_seen >= $fresh ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 7",
            ] {
                let profile = same_as_scan(&c, sql, &params);
                assert!(profile.metrics.spatial_rounds >= 1, "{sql}: {:?}", profile.metrics);
                // The dense cluster holds 40 eligible drivers. Elsewhere, fewer
                // than k eligible drivers may lie within 2,000 km, and the
                // statement then falls back to the ordinary plan.
                if (x, y) == (106.7, 10.8) && k <= 5 {
                    assert_eq!(profile.metrics.fullscan_steps, 0, "{sql}: {:?}", profile.metrics);
                }
            }
            // Eligible points far from most centers: complete either through
            // widening rounds or by falling back to the ordinary plan.
            same_as_scan(
                &c,
                "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND available = false AND last_seen = 7 AND id > drivers:2000 ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT $k",
                &params,
            );
        }
    }
    let params = Parameters::from([("$k".into(), Value::Integer(5))]);
    for sql in [
        "SELECT id FROM drivers WHERE available = true ORDER BY ST_DistanceSphere(location, ST_Point(0, 0)), id LIMIT $k",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, NULL) IS NULL ORDER BY ST_DistanceSphere(location, NULL), id LIMIT $k",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(0, 0)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point(0, 0)) DESC LIMIT $k",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(0, 0)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point(0, 0)), id LIMIT 0",
        "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(0, 0)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point(0, 0)), id LIMIT 5 OFFSET 2",
    ] {
        same_as_scan(&c, sql, &params);
        let used = params
            .iter()
            .filter(|(name, _)| sql.contains(name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Parameters>();
        let plan = format!("{:?}", c.execute(&format!("EXPLAIN QUERY PLAN {sql}"), &used).unwrap().rows);
        assert!(!plan.contains("drivers_location"), "{sql}: {plan}");
    }
}

fn zones(c: &romedb::Connection) -> Vec<(String, String)> {
    q(c, "CREATE TABLE zones");
    q(c, "BEGIN");
    let mut shapes = Vec::new();
    let mut seed = 77u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for i in 0..300 {
        let (x, y) = (next() * 340.0 - 170.0, next() * 150.0 - 75.0);
        let (w, h) = (0.01 + next() * 3.0, 0.01 + next() * 3.0);
        let shape = match i % 4 {
            0 => format!(
                r#"{{"type":"Polygon","coordinates":[[[{x},{y}],[{},{y}],[{},{}],[{x},{}],[{x},{y}]]]}}"#,
                x + w,
                x + w,
                y + h,
                y + h
            ),
            1 => format!(
                r#"{{"type":"Polygon","coordinates":[[[{x},{y}],[{},{y}],[{},{}],[{x},{y}]]]}}"#,
                x + w,
                x + w / 2.0,
                y + h
            ),
            2 => format!(
                r#"{{"type":"Polygon","coordinates":[[[{x},{y}],[{},{y}],[{},{}],[{x},{}],[{x},{y}]],[[{},{}],[{},{}],[{},{}],[{},{}],[{},{}]]]}}"#,
                x + w,
                x + w,
                y + h,
                y + h,
                x + w * 0.25,
                y + h * 0.25,
                x + w * 0.25,
                y + h * 0.75,
                x + w * 0.75,
                y + h * 0.75,
                x + w * 0.75,
                y + h * 0.25,
                x + w * 0.25,
                y + h * 0.25
            ),
            _ => format!(
                r#"{{"type":"MultiPolygon","coordinates":[[[[{x},{y}],[{},{y}],[{},{}],[{x},{y}]]],[[[{},{y}],[{},{y}],[{},{}],[{},{y}]]]]}}"#,
                x + w,
                x + w,
                y + h,
                x + w * 2.0,
                x + w * 3.0,
                x + w * 3.0,
                y + h,
                x + w * 2.0
            ),
        };
        q(c, &format!("INSERT INTO zones {{id:type::record('zones',{i}),shape:geo::from_geojson('{shape}')}}"));
        shapes.push((format!("{i}"), shape));
    }
    let seam = r#"{"type":"MultiPolygon","coordinates":[[[[170,-10],[180,-10],[180,10],[170,10],[170,-10]]],[[[-180,-10],[-170,-10],[-170,10],[-180,10],[-180,-10]]]]}"#;
    q(
        c,
        &format!("INSERT INTO zones {{id:zones:seam,shape:geo::from_geojson('{seam}')}}"),
    );
    q(c, "INSERT INTO zones {id:zones:empty}");
    q(c, "INSERT INTO zones {id:zones:nothing,shape:NULL}");
    q(
        c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    q(c, "COMMIT");
    shapes
}

#[test]
fn region_index_finds_containing_zones_like_a_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("zones.db");
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    zones(&c);
    let mut points = vec![
        (180.0, 0.0),
        (-180.0, 5.0),
        (175.0, 0.0),
        (0.0, 0.0),
        (179.0, 10.0),
    ];
    for row in q(&c, "SELECT shape FROM zones WHERE id = type::record('zones', 2) OR id = type::record('zones', 6) OR id = type::record('zones', 13)").rows {
        let Value::Object(shape) = &row[0] else { panic!("{row:?}") };
        let Value::Array(rings) = &shape["coordinates"] else { panic!() };
        let Value::Array(ring) = &rings[0] else { panic!() };
        for position in ring {
            let Value::Array(xy) = position else { panic!() };
            let (Value::Number(x), Value::Number(y)) = (&xy[0], &xy[1]) else { panic!() };
            points.push((*x, *y));
            points.push((*x + 0.001, *y + 0.001));
        }
    }
    let mut seed = 5u64;
    for _ in 0..150 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let x = (seed >> 11) as f64 / (1u64 << 53) as f64 * 340.0 - 170.0;
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let y = (seed >> 11) as f64 / (1u64 << 53) as f64 * 150.0 - 75.0;
        points.push((x, y));
    }
    let mut matched = 0;
    for (x, y) in points {
        let params = Parameters::from([
            ("$x".into(), Value::Number(x)),
            ("$y".into(), Value::Number(y)),
        ]);
        for (mode, predicate) in [
            ("covers", "ST_Covers(shape, ST_Point($x, $y))"),
            ("contains", "ST_Contains(shape, ST_Point($x, $y))"),
        ] {
            let scan = ids(c
                .execute(
                    &format!("SELECT id FROM zones NOT INDEXED WHERE {predicate}"),
                    &params,
                )
                .unwrap()
                .rows);
            let source = ids(c.execute(&format!("SELECT id FROM search::containing('zones_shape', ST_Point($x, $y), '{mode}')"), &params).unwrap().rows);
            assert_eq!(source, scan, "{mode} {x},{y}");
            let rewritten = c
                .profile_select(&format!("SELECT id FROM zones WHERE {predicate}"), &params)
                .unwrap();
            assert_eq!(ids(rewritten.result.rows), scan, "{mode} rewrite {x},{y}");
            assert_eq!(
                rewritten.metrics.fullscan_steps, 0,
                "{:?}",
                rewritten.metrics
            );
            matched += scan.len();
        }
        let within = ids(c
            .execute(
                "SELECT id FROM zones WHERE ST_Within(ST_Point($x, $y), shape)",
                &params,
            )
            .unwrap()
            .rows);
        let intersects = ids(c
            .execute(
                "SELECT id FROM zones WHERE ST_Intersects(ST_Point($x, $y), shape)",
                &params,
            )
            .unwrap()
            .rows);
        assert_eq!(
            within,
            ids(c
                .execute(
                    "SELECT id FROM zones NOT INDEXED WHERE ST_Contains(shape, ST_Point($x, $y))",
                    &params
                )
                .unwrap()
                .rows)
        );
        assert_eq!(
            intersects,
            ids(c
                .execute(
                    "SELECT id FROM zones NOT INDEXED WHERE ST_Covers(shape, ST_Point($x, $y))",
                    &params
                )
                .unwrap()
                .rows)
        );
    }
    assert!(matched > 10, "{matched}");
    let plan = format!(
        "{:?}",
        q(
            &c,
            "EXPLAIN QUERY PLAN SELECT id FROM zones WHERE ST_Covers(shape, ST_Point(1, 1))"
        )
        .rows
    );
    assert!(plan.contains("zones_shape"), "{plan}");
    c.check_collection_integrity("zones", Default::default())
        .unwrap();

    // Writes keep entries and shapes current, roll back atomically and survive reopening.
    q(
        &c,
        "UPDATE zones SET shape = ST_MakeEnvelope(10, 10, 11, 11) WHERE id = zones:seam",
    );
    assert_eq!(
        ids(q(
            &c,
            "SELECT id FROM search::containing('zones_shape', ST_Point(180, 0), 'covers')"
        )
        .rows),
        Vec::<String>::new()
    );
    assert_eq!(q(&c, "SELECT id FROM search::containing('zones_shape', ST_Point(10.5, 10.5), 'contains')").rows.len(), 1 + ids(q(&c, "SELECT id FROM zones NOT INDEXED WHERE ST_Contains(shape, ST_Point(10.5, 10.5)) AND id != zones:seam").rows).len());
    q(&c, "BEGIN");
    q(&c, "DELETE FROM zones WHERE id = zones:seam");
    q(
        &c,
        "INSERT INTO zones {id:zones:late,shape:ST_MakeEnvelope(50, 50, 51, 51)}",
    );
    q(&c, "ROLLBACK");
    assert!(q(
        &c,
        "SELECT id FROM search::containing('zones_shape', ST_Point(50.5, 50.5), 'covers')"
    )
    .rows
    .iter()
    .all(|row| format!("{:?}", row[0])
        != "Record(Record { table: \"zones\", key: String(\"late\") })"));
    q(&c, "DELETE FROM zones WHERE id = zones:seam");
    c.check_collection_integrity("zones", Default::default())
        .unwrap();
    drop(c);
    drop(db);
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    c.check_collection_integrity("zones", Default::default())
        .unwrap();
    q(&c, "REINDEX zones_shape");
    c.check_collection_integrity("zones", Default::default())
        .unwrap();
    assert_eq!(
        q(
            &c,
            "SELECT id FROM search::containing('zones_shape', ST_Point(10.5, 10.5), 'contains')"
        )
        .rows
        .len(),
        ids(q(
            &c,
            "SELECT id FROM zones NOT INDEXED WHERE ST_Contains(shape, ST_Point(10.5, 10.5))"
        )
        .rows)
        .len()
    );

    // Values, sources and declarations that do not fit a region index are rejected.
    assert_eq!(
        c.execute(
            "INSERT INTO zones {id:zones:point,shape:ST_Point(1, 1)}",
            &Parameters::new()
        )
        .unwrap_err()
        .code(),
        "FDB_VALIDATION"
    );
    for sql in [
        "SELECT id FROM search::near('zones_shape', ST_Point(0, 0), 10)",
        "SELECT id FROM search::bbox('zones_shape', 0, 0, 1, 1)",
        "SELECT id FROM search::containing('zones_shape', ST_Point(0, 0), 'within')",
        "SELECT id FROM search::containing('zones_shape', ST_MakeEnvelope(0, 0, 1, 1), 'covers')",
        "CREATE SEARCH INDEX IF NOT EXISTS zones_shape ON zones(shape) USING SPATIAL",
    ] {
        assert!(c.execute(sql, &Parameters::new()).is_err(), "{sql}");
    }
    q(&c, "CREATE SEARCH INDEX IF NOT EXISTS zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')");
    q(&c, "CREATE TABLE spots");
    q(&c, "INSERT INTO spots {id:spots:a,location:ST_Point(1, 1)}");
    assert!(c.execute("CREATE SEARCH INDEX spots_regions ON spots(location) USING SPATIAL WITH (geometry = 'region')", &Parameters::new()).is_err());
    q(
        &c,
        "CREATE SEARCH INDEX spots_location ON spots(location) USING SPATIAL",
    );
    assert!(c
        .execute(
            "SELECT id FROM search::containing('spots_location', ST_Point(1, 1), 'covers')",
            &Parameters::new()
        )
        .is_err());
    q(&c, "DROP INDEX zones_shape");
    c.check_collection_integrity("zones", Default::default())
        .unwrap();
}

fn fleet_drivers(c: &romedb::Connection) {
    q(c, "CREATE TABLE drivers");
    q(c, "BEGIN");
    for i in 0..4000u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 17001) as f64 / 100.0 - 85.0;
        let fleet = if i % 500 == 0 { "night" } else { "day" };
        let status = if i % 1300 == 0 { "repair" } else { "active" };
        q(c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{i}),fleet:'{fleet}',status:'{status}',location:geo::point({longitude},{latitude})}}"
        ));
    }
    q(
        c,
        "INSERT INTO drivers {id:drivers:missing,fleet:'night',status:'repair'}",
    );
    q(
        c,
        "CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL",
    );
    q(c, "CREATE INDEX drivers_fleet ON drivers(fleet)");
    q(c, "COMMIT");
}

fn plan(c: &romedb::Connection, sql: &str, all: &Parameters) -> String {
    let used = all
        .iter()
        .filter(|(name, _)| sql.contains(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Parameters>();
    format!(
        "{:?}",
        c.execute(&format!("EXPLAIN QUERY PLAN {sql}"), &used)
            .unwrap()
            .rows
    )
}

#[test]
fn spatial_rewrites_pick_scalar_indexes_and_scans_by_selectivity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    fleet_drivers(&c);
    for (x, y) in [(0.0, 0.0), (106.7, 10.8), (-179.9, 60.0)] {
        let params = Parameters::from([
            ("$x".into(), Value::Number(x)),
            ("$y".into(), Value::Number(y)),
            ("$fleet".into(), Value::String("night".into())),
        ]);
        // Eight night drivers: the scalar index selects fewer documents than
        // any spatial round, so the ordinary plan searches it instead.
        for sql in [
            "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND fleet = 'night' ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5",
            "SELECT id FROM drivers WHERE fleet = $fleet AND ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5",
            "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point($x, $y), 15000000) AND fleet IN ('night') ORDER BY id",
        ] {
            let profile = same_as_scan(&c, sql, &params);
            assert_eq!(profile.metrics.spatial_rounds, 0, "{sql}: {:?}", profile.metrics);
            // Index entries and documents for the bounded count and the statement:
            // execution read the fleet index, not spatial candidates.
            assert!(profile.metrics.rows_read < 100, "{sql}: {:?}", profile.metrics);
            // EXPLAIN reads no data, so it cannot count the fleet and shows the
            // spatial plan that execution would use without that count.
            let plan = plan(&c, sql, &params);
            assert!(plan.contains("drivers_location"), "{sql}: {plan}");
        }
        // Almost every driver is in the day fleet: the spatial candidates drive
        // the statement and the fleet index is not scanned.
        for sql in [
            "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND fleet = 'day' ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5",
            "SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point($x, $y), 300000) AND fleet = 'day' ORDER BY id",
        ] {
            let profile = same_as_scan(&c, sql, &params);
            assert!(profile.metrics.rows_read < 400, "{sql}: {:?}", profile.metrics);
            let plan = plan(&c, sql, &params);
            assert!(plan.contains("drivers_location") && !plan.contains("drivers_fleet"), "{sql}: {plan}");
        }
        // Three drivers in repair and no index on status: rounds stop once the
        // match rate shows that one ordinary scan is cheaper.
        let sql = "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point($x, $y)) IS NOT NULL AND status = 'repair' ORDER BY ST_DistanceSphere(location, ST_Point($x, $y)), id LIMIT 5";
        let profile = same_as_scan(&c, sql, &params);
        assert!(
            profile.metrics.fullscan_steps > 0,
            "{sql}: {:?}",
            profile.metrics
        );
    }
}

#[test]
fn nearest_rounds_stop_on_cancellation_deadlines_and_limits() {
    use romedb::{CancellationToken, ResultLimits, TransactionState};
    use std::time::{Duration, Instant};
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE drivers");
    q(&c, "BEGIN");
    for i in 0..20_000u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 17001) as f64 / 100.0 - 85.0;
        let status = if i % 4000 == 0 { "repair" } else { "active" };
        q(&c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{i}),status:'{status}',location:geo::point({longitude},{latitude})}}"
        ));
    }
    q(
        &c,
        "CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL",
    );
    q(&c, "COMMIT");
    let p = Parameters::new();
    let rare = "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(10, 10)) IS NOT NULL AND status = 'repair' ORDER BY ST_DistanceSphere(location, ST_Point(10, 10)), id LIMIT 5";
    let explicit = "SELECT id FROM search::nearest('drivers_location', ST_Point(10, 10), 10000)";
    let expected = c
        .execute(
            &rare.replacen("FROM drivers", "FROM drivers NOT INDEXED", 1),
            &p,
        )
        .unwrap()
        .rows;
    assert_eq!(expected.len(), 5);

    q(&c, "BEGIN");
    q(
        &c,
        "INSERT INTO drivers {id:drivers:pending,status:'repair',location:geo::point(10,10)}",
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    for sql in [rare, explicit] {
        let error = c.execute_cancellable(sql, &p, &cancelled).unwrap_err();
        assert_eq!(error.code(), "FDB_CANCELLED", "{sql}");
        // A deadline that expires during the index rounds and document probes.
        let token = CancellationToken::with_deadline(Instant::now() + Duration::from_millis(5));
        if let Err(error) = c.execute_cancellable(sql, &p, &token) {
            assert_eq!(error.code(), "FDB_CANCELLED", "{sql}");
        }
        assert_eq!(c.transaction_state(), TransactionState::Active);
    }
    // The open transaction keeps its write and every probe left no state behind.
    let rows = c.execute(rare, &p).unwrap().rows;
    assert_eq!(
        rows[0],
        vec![Value::Record(romedb::Record {
            table: "drivers".into(),
            key: romedb::Key::String("pending".into()),
        })]
    );
    assert_eq!(rows[1..], expected[..4]);
    q(&c, "ROLLBACK");
    assert_eq!(c.execute(rare, &p).unwrap().rows, expected);

    // Limits fail the whole statement without partial output.
    let limits = ResultLimits {
        max_rows: 3,
        max_payload_bytes: 1 << 20,
    };
    for sql in [rare, explicit] {
        let error = c.select_with_limits(sql, &p, limits).unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT", "{sql}");
    }
    for k in ["0", "10001", "2.5", "-1"] {
        let error = c
            .execute(
                &format!(
                    "SELECT id FROM search::nearest('drivers_location', ST_Point(10, 10), {k})"
                ),
                &p,
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION", "{k}");
    }
    // Above the nearest limit, the ordinary form keeps ordinary SQL semantics.
    let all = "SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(10, 10)) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point(10, 10)), id LIMIT 10001";
    let profile = c.profile_select(all, &p).unwrap();
    assert_eq!(profile.result.rows.len(), 10001);
    assert_eq!(profile.metrics.spatial_rounds, 0);
}

/// Deterministic fixture for the frozen work ceilings: 10,000 points in 20
/// cities of about 5 km spread, 10,000 points uniform over the sphere, and 200
/// rectangular zones of 0.02 to 1 degree.
fn ceiling_fixture(c: &romedb::Connection) {
    let mut seed = 0x0ce1u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let cities = (0..20)
        .map(|_| (next() * 300.0 - 150.0, next() * 120.0 - 60.0))
        .collect::<Vec<_>>();
    q(c, "CREATE TABLE people");
    q(c, "CREATE TABLE zones");
    q(c, "BEGIN");
    for i in 0..20_000usize {
        let (x, y) = if i < 10_000 {
            let (cx, cy) = cities[i % 20];
            (cx + (next() - 0.5) * 0.1, cy + (next() - 0.5) * 0.1)
        } else {
            (
                next() * 360.0 - 180.0,
                (2.0 * next() - 1.0).asin().to_degrees(),
            )
        };
        q(c, &format!(
            "INSERT INTO people {{id:type::record('people',{i}),available:{},location:geo::point({x},{y})}}",
            i % 3 == 0
        ));
    }
    for i in 0..200 {
        let (cx, cy) = cities[i % 20];
        let (x, y) = (cx + (next() - 0.5) * 2.0, cy + (next() - 0.5) * 2.0);
        let size = 0.02 + next();
        q(c, &format!(
            "INSERT INTO zones {{id:type::record('zones',{i}),shape:ST_MakeEnvelope({x},{y},{},{})}}",
            x + size,
            y + size
        ));
    }
    q(
        c,
        "CREATE SEARCH INDEX people_location ON people(location) USING SPATIAL",
    );
    q(
        c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    q(c, "COMMIT");
}

#[test]
fn selective_fixture_work_stays_under_frozen_ceilings() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    ceiling_fixture(&c);
    // Query point inside the first city, and one in open ocean.
    let city = c
        .execute(
            "SELECT ST_X(location), ST_Y(location) FROM people:0",
            &Parameters::new(),
        )
        .unwrap()
        .rows;
    let (Value::Number(x), Value::Number(y)) = (&city[0][0], &city[0][1]) else {
        panic!("{city:?}")
    };
    let (x, y) = (*x, *y);
    // Each row: query, and ceilings on index entries read, documents read,
    // key ranges and nearest rounds, measured on this fixture with headroom.
    let cases: Vec<(String, [u64; 4])> = vec![
        (format!("SELECT id FROM search::near('people_location', ST_Point({x}, {y}), 1000)"), [65, 80, 8, 0]),
        (format!("SELECT id FROM people WHERE ST_DWithin(location, ST_Point({x}, {y}), 1000)"), [135, 215, 8, 0]),
        (format!("SELECT id FROM search::bbox('people_location', {}, {}, {}, {})", x - 0.01, y - 0.01, x + 0.01, y + 0.01), [35, 35, 4, 0]),
        (format!("SELECT id FROM people WHERE ST_Covers(ST_MakeEnvelope({}, {}, {}, {}), location)", x - 0.01, y - 0.01, x + 0.01, y + 0.01), [80, 130, 4, 0]),
        (format!("SELECT id FROM search::nearest('people_location', ST_Point({x}, {y}), 5)"), [50, 10, 40, 5]),
        (format!("SELECT id FROM people WHERE ST_DistanceSphere(location, ST_Point({x}, {y})) IS NOT NULL AND available = true ORDER BY ST_DistanceSphere(location, ST_Point({x}, {y})), id LIMIT 5"), [150, 250, 90, 8]),
        ("SELECT id FROM people WHERE ST_DWithin(location, ST_Point(-140, -40), 50000)".to_owned(), [10, 10, 6, 0]),
        ("SELECT id FROM search::nearest('people_location', ST_Point(-140, -40), 5)".to_owned(), [50, 10, 64, 10]),
        (format!("SELECT id FROM zones WHERE ST_Covers(shape, ST_Point({x}, {y}))"), [50, 75, 2, 0]),
        (format!("SELECT id FROM search::containing('zones_shape', ST_Point({x}, {y}), 'covers')"), [50, 75, 2, 0]),
    ];
    let mut failures = Vec::new();
    for (sql, ceiling) in &cases {
        let m = c.profile_select(sql, &Parameters::new()).unwrap().metrics;
        let work = [
            m.index_steps,
            m.rows_read,
            m.spatial_ranges,
            m.spatial_rounds,
        ];
        eprintln!("{work:?} {sql}");
        if work
            .iter()
            .zip(ceiling)
            .any(|(work, ceiling)| work > ceiling)
        {
            failures.push(format!("{work:?} > {ceiling:?}: {sql}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn randomized_writes_keep_indexed_results_equal_to_scans() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    let mut seed = 0x51ce_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // Coordinates cluster on the antimeridian, the poles, the equator and the
    // prime meridian, where covers split or degenerate, plus uniform points.
    let mut coordinate = {
        let mut seed = 0xc00d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        move || -> (f64, f64) {
            let unit = |n: u64| (n >> 11) as f64 / (1u64 << 53) as f64;
            let jitter = (unit(next()) - 0.5) * 0.02;
            match next() % 6 {
                0 => (
                    if next() % 2 == 0 {
                        180.0 - jitter.abs()
                    } else {
                        -180.0 + jitter.abs()
                    },
                    unit(next()) * 160.0 - 80.0,
                ),
                1 => (
                    unit(next()) * 360.0 - 180.0,
                    if next() % 2 == 0 {
                        90.0 - jitter.abs()
                    } else {
                        -90.0 + jitter.abs()
                    },
                ),
                2 => (jitter, jitter),
                3 => ((next() % 7) as f64 - 3.0, (next() % 5) as f64 - 2.0),
                _ => (unit(next()) * 360.0 - 180.0, unit(next()) * 180.0 - 90.0),
            }
        }
    };
    q(&c, "CREATE TABLE places");
    q(&c, "CREATE TABLE zones");
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    q(
        &c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    let point_sql = |(x, y): (f64, f64)| format!("geo::point({x},{y})");
    let check = |c: &romedb::Connection, round: usize, center: (f64, f64), other: (f64, f64)| {
        let (x, y) = center;
        // Envelopes stay narrower than 180 degrees inside the latitude range.
        let w = x.clamp(-179.5, 120.0);
        let e = (w + 0.5 + other.0.abs() / 4.0).min(179.9);
        let s = y.clamp(-89.5, 60.0);
        let n = (s + 0.5 + other.1.abs() / 4.0).min(89.9);
        let band = (other.1.abs() / 2.0).clamp(1.0, 80.0);
        let queries = [
            format!("SELECT id FROM places WHERE ST_DWithin(location, ST_Point({x}, {y}), 500000) ORDER BY id"),
            format!("SELECT id FROM places WHERE ST_DistanceSphere(location, ST_Point({x}, {y})) IS NOT NULL ORDER BY ST_DistanceSphere(location, ST_Point({x}, {y})), id LIMIT 7"),
            format!("SELECT id FROM places WHERE ST_Covers(ST_MakeEnvelope({w}, {s}, {e}, {n}), location) ORDER BY id"),
            format!("SELECT id FROM places WHERE ST_Intersects(location, ST_MakeEnvelope({w}, {s}, {e}, {n})) ORDER BY id"),
            format!("SELECT id FROM zones WHERE ST_Covers(shape, ST_Point({x}, {y})) ORDER BY id"),
            format!("SELECT id FROM zones WHERE ST_Contains(shape, ST_Point({x}, {y})) ORDER BY id"),
        ];
        for sql in queries {
            let indexed = c.execute(&sql, &Parameters::new()).expect(&sql).rows;
            let scan = sql
                .replacen("FROM places", "FROM places NOT INDEXED", 1)
                .replacen("FROM zones", "FROM zones NOT INDEXED", 1);
            let expected = c.execute(&scan, &Parameters::new()).expect(&scan).rows;
            assert_eq!(indexed, expected, "round {round}: {sql}");
        }
        // A viewport across the antimeridian equals its two halves.
        let crossing = format!(
            "SELECT id FROM search::bbox('places_location', 170, {}, -170, {band}) ORDER BY id",
            -band
        );
        let halves = format!(
            "SELECT id FROM places NOT INDEXED WHERE ST_Covers(ST_MakeEnvelope(170, {s}, 180, {band}), location) OR ST_Covers(ST_MakeEnvelope(-180, {s}, -170, {band}), location) ORDER BY id",
            s = -band
        );
        assert_eq!(
            c.execute(&crossing, &Parameters::new()).unwrap().rows,
            c.execute(&halves, &Parameters::new()).unwrap().rows,
            "round {round}: {crossing}"
        );
    };
    let mut live_points = 0usize;
    for round in 0..120usize {
        let rollback = round % 9 == 4;
        q(&c, "BEGIN");
        for _ in 0..25 {
            let id = next() % 400;
            match next() % 10 {
                0..=3 => {
                    q(
                        &c,
                        &format!(
                            "UPSERT places:{id} {{location:{}}}",
                            point_sql(coordinate())
                        ),
                    );
                }
                4 => {
                    q(
                        &c,
                        &format!("UPDATE places SET location = NULL WHERE id = places:{id}"),
                    );
                }
                5 => {
                    q(&c, &format!("DELETE FROM places WHERE id = places:{id}"));
                }
                6 => {
                    q(&c, &format!("UPSERT places:{id} {{name:'no location'}}"));
                }
                7 | 8 => {
                    let (x, y) = coordinate();
                    let (x, y) = (x.clamp(-179.0, 176.0), y.clamp(-89.0, 86.0));
                    let (dx, dy) = (
                        0.001 + (next() % 1000) as f64 / 400.0,
                        0.001 + (next() % 1000) as f64 / 400.0,
                    );
                    q(
                        &c,
                        &format!(
                            "UPSERT zones:{} {{shape:ST_MakeEnvelope({x},{y},{},{})}}",
                            id % 60,
                            x + dx,
                            y + dy
                        ),
                    );
                }
                _ => {
                    q(
                        &c,
                        &format!("DELETE FROM zones WHERE id = zones:{}", id % 60),
                    );
                }
            }
        }
        if rollback {
            q(&c, "ROLLBACK");
        } else {
            q(&c, "COMMIT");
        }
        if round % 4 == 0 {
            check(&c, round, coordinate(), coordinate());
        }
        let counted = q(
            &c,
            "SELECT count(*) FROM places WHERE ST_X(location) IS NOT NULL",
        )
        .rows;
        let [Value::Integer(n)] = counted[0].as_slice() else {
            panic!("{counted:?}")
        };
        live_points = *n as usize;
    }
    assert!(live_points > 0);
    for table in ["places", "zones"] {
        c.check_collection_integrity(table, Default::default())
            .unwrap();
    }
}

#[test]
fn info_reports_spatial_index_geometry_and_format() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE places");
    q(&c, "CREATE TABLE zones");
    q(
        &c,
        "CREATE SEARCH INDEX places_location ON places(location) USING SPATIAL",
    );
    q(
        &c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    for (sql, geometry) in [
        ("INFO FOR INDEX places_location", "point"),
        ("INFO FOR INDEX zones_shape", "region"),
    ] {
        let info = format!("{:?}", q(&c, sql).rows);
        assert!(
            info.contains(&format!("\"geometry\": String(\"{geometry}\")")),
            "{sql}: {info}"
        );
        assert!(
            info.contains("\"format\": String(\"two_dimensional\")"),
            "{sql}: {info}"
        );
    }
    let table = format!("{:?}", q(&c, "INFO FOR TABLE zones").rows);
    assert!(
        table.contains("\"geometry\": String(\"region\")"),
        "{table}"
    );
}
