use romedb::{Database, Parameters, Value};
fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new()).unwrap()
}
#[test]
fn profiles_measure_scan_reduction_without_changing_typed_results() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE docs");
    q(&c, "BEGIN");
    for n in 0..200 {
        q(&c,&format!("INSERT INTO docs (id,group_no,payload,flag) VALUES (type::record('docs',{n}),{},X'02ff',true)",n%20));
    }
    q(&c, "COMMIT");
    let sql = "SELECT id,payload,flag FROM docs WHERE group_no=$group ORDER BY id";
    let params = Parameters::from([("$group".into(), Value::Integer(7))]);
    let scan = c.profile_select(sql, &params).unwrap();
    assert_eq!(scan.result.rows, c.execute(sql, &params).unwrap().rows);
    assert_eq!(scan.result.rows.len(), 10);
    assert!(scan.metrics.fullscan_steps >= 199, "{:?}", scan.metrics);
    assert!(scan.metrics.vm_steps > 0);
    assert_eq!(scan.metrics.rows_written, 0);
    q(&c, "CREATE INDEX docs_group ON docs(group_no)");
    let indexed = c.profile_select(sql, &params).unwrap();
    assert_eq!(indexed.result.rows, scan.result.rows);
    assert_eq!(indexed.result.columns, scan.result.columns);
    assert!(
        indexed.metrics.fullscan_steps < scan.metrics.fullscan_steps,
        "{:?}",
        indexed.metrics
    );
    assert!(
        indexed.metrics.rows_read < scan.metrics.rows_read,
        "{:?} vs {:?}",
        indexed.metrics,
        scan.metrics
    );
    assert_eq!(
        c.profile_select(sql, &params).unwrap().metrics,
        indexed.metrics
    );
    let nested=c.profile_select("WITH a AS (SELECT id,payload,flag FROM docs WHERE group_no=$group) SELECT * FROM a ORDER BY id",&params).unwrap();
    assert_eq!(nested.result.rows, indexed.result.rows);
    assert!(nested.metrics.vm_steps > 0);
}
#[test]
fn native_profiles_bind_values_and_reject_writes_before_execution() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE native(n INTEGER)");
    q(&c, "INSERT INTO native VALUES (1),(2),(3)");
    let sql = "SELECT n FROM native WHERE n>$n ORDER BY n";
    let params = Parameters::from([("$n".into(), Value::Integer(1))]);
    let profile = c.profile_select(sql, &params).unwrap();
    assert_eq!(profile.result.rows, c.execute(sql, &params).unwrap().rows);
    assert!(profile.metrics.rows_read >= 3, "{:?}", profile.metrics);
    assert!(profile.metrics.fullscan_steps >= 2);
    for sql in [
        "DELETE FROM native",
        "CREATE TABLE docs",
        "SELECT 1; DELETE FROM native",
        "SELECT __romedb_pack(n) FROM native",
    ] {
        assert!(c.profile_select(sql, &Parameters::new()).is_err(), "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT count(*) FROM native").rows,
        vec![vec![Value::Integer(3)]]
    );
    q(&c, "CREATE TABLE docs");
    q(&c, "INSERT INTO docs {id:docs:a,ref:docs:a}");
    let profile = c
        .profile_select("SELECT record::fetch(ref) FROM docs", &Parameters::new())
        .unwrap();
    assert_eq!(
        profile.result.rows,
        q(&c, "SELECT record::fetch(ref) FROM docs").rows
    );
    assert_eq!(profile.metrics.fetch_batches, 1);
}

#[test]
fn profiles_count_successful_seeks_and_deferred_table_visits() {
    for on_disk in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seek-profile.db");
        let db = Database::open(if on_disk {
            path.to_str().unwrap()
        } else {
            ":memory:"
        })
        .unwrap();
        let c = db.connect().unwrap();
        q(
            &c,
            "CREATE TABLE seek_items(id INTEGER PRIMARY KEY, n INTEGER, payload TEXT)",
        );
        q(&c, "BEGIN");
        for n in 0..40 {
            q(
                &c,
                &format!("INSERT INTO seek_items VALUES({n},{n},'value-{n}')"),
            );
        }
        q(&c, "COMMIT");
        q(&c, "CREATE INDEX seek_items_n ON seek_items(n)");
        let cases = [
            ("SELECT payload FROM seek_items WHERE id=17", 1),
            ("SELECT payload FROM seek_items WHERE id=100", 0),
            (
                "SELECT payload FROM seek_items WHERE id>=17 ORDER BY id LIMIT 1",
                1,
            ),
            (
                "SELECT payload FROM seek_items WHERE id<=17 ORDER BY id DESC LIMIT 1",
                1,
            ),
            (
                "SELECT n FROM seek_items INDEXED BY seek_items_n WHERE n=17 LIMIT 1",
                1,
            ),
            (
                "SELECT payload FROM seek_items INDEXED BY seek_items_n WHERE n=17 LIMIT 1",
                2,
            ),
            (
                "SELECT payload FROM seek_items INDEXED BY seek_items_n WHERE n=100 LIMIT 1",
                0,
            ),
            (
                "SELECT count(*) FROM seek_items a JOIN seek_items b ON a.id=b.id WHERE a.n>=0",
                80,
            ),
        ];
        for (sql, expected) in cases {
            let profile = c.profile_select(sql, &Parameters::new()).unwrap();
            assert_eq!(profile.result.rows, q(&c, sql).rows, "{sql}");
            assert_eq!(
                profile.metrics.rows_read, expected,
                "{sql}; disk={on_disk}; {:?}",
                profile.metrics
            );
        }
    }
}

#[test]
fn hash_join_counts_table_visits_once() {
    for on_disk in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hash-profile.db");
        let db = Database::open(if on_disk {
            file.to_str().unwrap()
        } else {
            ":memory:"
        })
        .unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE hash_left(n INTEGER)");
        q(&c, "CREATE TABLE hash_right(n INTEGER)");
        for modulus in [100, 10] {
            q(&c, "BEGIN");
            q(&c, "DELETE FROM hash_left");
            q(&c, "DELETE FROM hash_right");
            for n in 0..100 {
                q(
                    &c,
                    &format!("INSERT INTO hash_left VALUES({})", n % modulus),
                );
                q(
                    &c,
                    &format!("INSERT INTO hash_right VALUES({})", n % modulus),
                );
            }
            q(&c, "COMMIT");
            let sql = "SELECT count(*) FROM hash_left a JOIN hash_right b ON a.n=b.n";
            let explain = q(&c, &format!("EXPLAIN {sql}"));
            let opcodes: Vec<_> = explain.rows.iter().map(|row| row[1].clone()).collect();
            assert!(
                opcodes.contains(&Value::String("HashBuild".into())),
                "{opcodes:?}"
            );
            assert!(
                opcodes.contains(&Value::String("HashProbe".into())),
                "{opcodes:?}"
            );
            let profile = c.profile_select(sql, &Parameters::new()).unwrap();
            assert_eq!(
                profile.result.rows,
                vec![vec![Value::Integer(10000 / modulus)]]
            );
            assert_eq!(profile.result.rows, q(&c, sql).rows);
            // Two 100-row table scans; hash copies/probes do not reposition
            // either source cursor. Duplicate matches still visit each source
            // row once. Any spilled source seeks would be extra visits.
            assert_eq!(profile.metrics.btree_seeks, 0, "{:?}", profile.metrics);
            // The new planner does not classify hash-input iteration as full-scan steps.
            assert_eq!(profile.metrics.fullscan_steps, 0);
            assert_eq!(
                profile.metrics.rows_read, 200,
                "disk={on_disk}; modulus={modulus}; {:?}",
                profile.metrics
            );
        }
    }
}
