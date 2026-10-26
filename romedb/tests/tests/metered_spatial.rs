use romedb::{Database, Parameters, ReadWorkLimits, ResultLimits};

fn q(c: &romedb::Connection, sql: &str) {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

fn read(c: &romedb::Connection, sql: &str, max: Option<u64>) -> romedb::MeteredRead {
    c.select_metered(
        sql,
        &Parameters::new(),
        ResultLimits {
            max_rows: 1000,
            max_payload_bytes: 1 << 20,
        },
        ReadWorkLimits {
            max_rows_read: max,
            max_vm_steps: None,
        },
    )
}

#[test]
fn nearest_rounds_and_planning_reads_count_toward_metered_work() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE drivers");
    q(&c, "CREATE TABLE zones");
    q(&c, "BEGIN");
    for i in 0..3000u64 {
        let longitude = ((i * 7919) % 36001) as f64 / 100.0 - 180.0;
        let latitude = ((i * 104_729) % 17001) as f64 / 100.0 - 85.0;
        let fleet = if i % 600 == 0 { "night" } else { "day" };
        q(&c, &format!(
            "INSERT INTO drivers {{id:type::record('drivers',{i}),available:{},fleet:'{fleet}',status:'{fleet}',location:geo::point({longitude},{latitude})}}",
            i % 3 == 0
        ));
    }
    q(
        &c,
        "INSERT INTO zones {id:zones:a,shape:ST_MakeEnvelope(0, 0, 20, 20)}",
    );
    q(
        &c,
        "CREATE SEARCH INDEX drivers_location ON drivers(location) USING SPATIAL",
    );
    q(
        &c,
        "CREATE SEARCH INDEX zones_shape ON zones(shape) USING SPATIAL WITH (geometry = 'region')",
    );
    q(&c, "CREATE INDEX drivers_fleet ON drivers(fleet)");
    q(&c, "COMMIT");
    let nearest = |filter: &str| {
        format!("SELECT id FROM drivers WHERE ST_DistanceSphere(location, ST_Point(10, 10)) IS NOT NULL AND {filter} ORDER BY ST_DistanceSphere(location, ST_Point(10, 10)), id LIMIT 5")
    };
    // Index rounds, probes and bounded counts run before the final statement
    // and are charged to the same budget.
    for (sql, planned) in [
        ("SELECT id FROM search::nearest('drivers_location', ST_Point(10, 10), 5)".to_owned(), true),
        (nearest("available = true"), true),
        (nearest("status = 'night'"), true),
        (nearest("fleet = 'night'"), true),
        (nearest("fleet = 'day'"), true),
        ("SELECT id FROM drivers WHERE ST_DWithin(location, ST_Point(10, 10), 2000000) AND fleet = 'day' ORDER BY id".to_owned(), true),
        ("SELECT id FROM zones WHERE ST_Covers(shape, ST_Point(10, 10))".to_owned(), false),
    ] {
        let profile = c.profile_select(&sql, &Parameters::new()).unwrap();
        let complete = read(&c, &sql, None);
        assert_eq!(complete.outcome.unwrap().rows, profile.result.rows, "{sql}");
        let used = complete.work.rows_read;
        if planned {
            assert!(used > profile.metrics.rows_read, "{sql}: {used} <= {:?}", profile.metrics);
        } else {
            assert!(used >= profile.metrics.rows_read, "{sql}: {used} < {:?}", profile.metrics);
        }
        let exact = read(&c, &sql, Some(used));
        assert_eq!(exact.outcome.unwrap().rows, profile.result.rows, "{sql}");
        let short = read(&c, &sql, Some(used - 1));
        assert_eq!(short.outcome.unwrap_err().code(), "FDB_CANCELLED", "{sql}");
        assert!(short.work.read_budget_exhausted, "{sql}");
    }
}
