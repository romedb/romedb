use romedb::{Database, GraphLimits, Key, Parameters, Record, Value};

fn q(c: &romedb::Connection, sql: &str) -> romedb::QueryResult {
    c.execute(sql, &Parameters::new())
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}
fn fails(c: &romedb::Connection, sql: &str) -> romedb::Error {
    match c.execute(sql, &Parameters::new()) {
        Ok(result) => panic!("{sql} unexpectedly returned {:?}", result.rows),
        Err(error) => error,
    }
}
fn rec(table: &str, key: &str) -> Value {
    Value::Record(Record {
        table: table.into(),
        key: Key::String(key.into()),
    })
}
fn keys(rows: &[Vec<Value>]) -> Vec<String> {
    rows.iter()
        .map(|row| match &row[0] {
            Value::Record(Record {
                key: Key::String(key),
                ..
            }) => key.clone(),
            other => panic!("{other:?}"),
        })
        .collect()
}
fn social(c: &romedb::Connection) {
    for sql in [
        "CREATE TABLE people",
        "CREATE TABLE articles",
        "CREATE EDGE TABLE follows FROM people TO people",
        "CREATE EDGE TABLE wrote FROM people TO articles ON DELETE CASCADE",
        "INSERT INTO people:alice { name: 'Alice', active: true }",
        "INSERT INTO people:bob { name: 'Bob', active: true }",
        "INSERT INTO people:carol { name: 'Carol', active: false }",
        "INSERT INTO people:dave { name: 'Dave', active: true }",
        "INSERT INTO articles:a1 { title: 'Graphs' }",
        "INSERT INTO articles:a2 { title: 'Tables' }",
        "RELATE people:alice -> follows:ab -> people:bob { weight: 5 }",
        "RELATE people:alice -> follows:ac -> people:carol { weight: 1 }",
        "RELATE people:bob -> follows:bd -> people:dave { weight: 3 }",
        "RELATE people:carol -> follows:cd -> people:dave { weight: 9 }",
        "RELATE people:dave -> follows:da -> people:alice { weight: 2 }",
        "RELATE people:bob -> wrote:w1 -> articles:a1",
        "RELATE people:dave -> wrote:w2 -> articles:a2",
        "RELATE people:dave -> wrote:w3 -> articles:a1",
    ] {
        q(c, sql);
    }
}

#[test]
fn edge_declarations_require_endpoints_and_persist_with_info() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        assert_eq!(
            fails(&c, "CREATE EDGE TABLE follows FROM people TO people").code(),
            "FDB_NOT_FOUND"
        );
        q(&c, "CREATE TABLE people");
        q(&c, "BEGIN");
        q(&c, "CREATE EDGE TABLE follows FROM people TO people");
        q(&c, "ROLLBACK");
        assert!(c
            .execute("INFO FOR TABLE follows", &Parameters::new())
            .is_err());
        q(&c, "CREATE EDGE TABLE follows FROM people TO people");
        q(
            &c,
            "CREATE EDGE TABLE IF NOT EXISTS follows FROM people TO people",
        );
        assert_eq!(
            fails(
                &c,
                "CREATE EDGE TABLE IF NOT EXISTS follows FROM people TO people ON DELETE CASCADE"
            )
            .code(),
            "FDB_ALREADY_EXISTS"
        );
        assert_eq!(
            fails(&c, "CREATE EDGE TABLE meta FROM follows TO people").code(),
            "FDB_UNSUPPORTED"
        );
        assert_eq!(
            fails(&c, "DEFINE FIELD source ON follows TYPE string").code(),
            "FDB_VALIDATION"
        );
        assert_eq!(fails(&c, "DROP TABLE people").code(), "FDB_VALIDATION");
    }
    let db = Database::open(path.to_str().unwrap()).unwrap();
    let c = db.connect().unwrap();
    let Value::Object(info) = &q(&c, "INFO FOR TABLE follows").rows[0][0] else {
        panic!()
    };
    assert_eq!(info["kind"], Value::String("edge".into()));
    let Value::Object(edge) = &info["edge"] else {
        panic!()
    };
    assert_eq!(edge["source"], Value::String("people".into()));
    assert_eq!(edge["on_delete"], Value::String("restrict".into()));
    let Value::Object(people) = &q(&c, "INFO FOR TABLE people").rows[0][0] else {
        panic!()
    };
    assert_eq!(
        people["edge_tables"],
        Value::Array(vec![Value::String("follows".into())])
    );
    q(&c, "DROP TABLE follows");
    q(&c, "DROP TABLE people");
    drop(c);
    drop(db);
    let db = Database::open(path.to_str().unwrap()).unwrap();
    db.connect().unwrap();
}

#[test]
fn relate_enforces_endpoint_existence_types_and_identity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    let created = q(
        &c,
        "RELATE people:carol -> follows -> people:bob { since: '2026-10-12' } RETURNING *",
    );
    let Value::Object(doc) = &created.rows[0][0] else {
        panic!()
    };
    assert_eq!(doc["source"], rec("people", "carol"));
    assert_eq!(doc["target"], rec("people", "bob"));
    assert!(matches!(&doc["id"], Value::Record(r) if r.table == "follows"));
    let mut params = Parameters::new();
    params.insert("$s".into(), Value::String("bob".into()));
    params.insert("$t".into(), rec("people", "carol"));
    params.insert(
        "$p".into(),
        Value::Object([("weight".into(), Value::Integer(4))].into()),
    );
    let row = c
        .execute(
            "RELATE type::record('people', $s) -> follows:bc -> $t DOCUMENT $p RETURNING weight",
            &params,
        )
        .unwrap();
    assert_eq!(row.rows, vec![vec![Value::Integer(4)]]);
    for (sql, code) in [
        (
            "RELATE people:zed -> follows -> people:bob",
            "FDB_CONSTRAINT",
        ),
        (
            "RELATE people:bob -> follows -> people:zed",
            "FDB_CONSTRAINT",
        ),
        (
            "RELATE people:bob -> follows -> articles:a1",
            "FDB_VALIDATION",
        ),
        (
            "RELATE people:bob -> people -> people:alice",
            "FDB_VALIDATION",
        ),
        (
            "RELATE people:bob -> nothing -> people:alice",
            "FDB_NOT_FOUND",
        ),
        (
            "RELATE people:alice -> follows:ab -> people:bob",
            "FDB_CONSTRAINT",
        ),
        (
            "RELATE people:bob -> follows -> people:alice { source: people:dave }",
            "FDB_VALIDATION",
        ),
        ("INSERT INTO follows { weight: 1 }", "FDB_VALIDATION"),
        (
            "INSERT INTO follows { source: people:bob, target: 'people:alice' }",
            "FDB_VALIDATION",
        ),
    ] {
        assert_eq!(fails(&c, sql).code(), code, "{sql}");
    }
    q(
        &c,
        "INSERT INTO follows { id: follows:sql, source: people:dave, target: people:bob }",
    );
    q(&c, "UPDATE follows SET weight = 7 WHERE id = follows:sql");
    q(&c, "UPDATE follows:sql MERGE { note: 'x' }");
    q(&c, "UPDATE follows:sql CONTENT { weight: 8 }");
    let doc = q(
        &c,
        "SELECT source, target, weight, note FROM follows WHERE id = follows:sql",
    );
    assert_eq!(
        doc.rows,
        vec![vec![
            rec("people", "dave"),
            rec("people", "bob"),
            Value::Integer(8),
            Value::Null
        ]]
    );
    for sql in [
        "UPDATE follows SET source = people:alice WHERE id = follows:sql",
        "UPDATE follows:sql MERGE { target: people:carol }",
        "UPDATE follows UNSET target WHERE id = follows:sql",
        "UPSERT follows:sql { source: people:alice }",
        "INSERT OR REPLACE INTO follows (id, source, target) VALUES (follows:sql, people:alice, people:bob)",
    ] {
        assert_eq!(fails(&c, sql).code(), "FDB_VALIDATION", "{sql}");
    }
    q(
        &c,
        "INSERT OR REPLACE INTO follows (id, source, target, weight) VALUES (follows:sql, people:dave, people:bob, 1)",
    );
    q(&c, "DELETE FROM follows WHERE id = follows:sql");
    let report = c
        .check_collection_integrity("follows", Default::default())
        .unwrap();
    assert_eq!(report.documents, 7);
    assert_eq!(report.index_entries, 14);
}

#[test]
fn deletion_policies_restrict_and_cascade_in_one_transaction() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    assert_eq!(
        fails(&c, "DELETE FROM people WHERE id = people:carol").code(),
        "FDB_CONSTRAINT"
    );
    assert_eq!(
        fails(
            &c,
            "INSERT OR REPLACE INTO people (id, name) VALUES (people:carol, 'C')"
        )
        .code(),
        "FDB_CONSTRAINT"
    );
    q(&c, "UPDATE people SET name = 'Caz' WHERE id = people:carol");
    q(&c, "UPSERT people:carol { name: 'Carol' }");
    q(
        &c,
        "DELETE FROM follows WHERE source = people:carol OR target = people:carol",
    );
    q(&c, "DELETE FROM people WHERE id = people:carol");
    assert_eq!(
        q(&c, "SELECT count(*) FROM people").rows[0][0],
        Value::Integer(3)
    );
    q(&c, "DELETE FROM articles WHERE id = articles:a1");
    assert_eq!(
        keys(&q(&c, "SELECT id FROM wrote").rows),
        vec!["w2".to_string()]
    );
    q(&c, "BEGIN");
    q(&c, "DELETE FROM articles WHERE id = articles:a2");
    assert_eq!(
        q(&c, "SELECT count(*) FROM wrote").rows[0][0],
        Value::Integer(0)
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        q(&c, "SELECT count(*) FROM wrote").rows[0][0],
        Value::Integer(1)
    );
    for table in ["people", "articles", "follows", "wrote"] {
        c.check_collection_integrity(table, Default::default())
            .unwrap_or_else(|e| panic!("{table}: {e}"));
    }
}

#[test]
fn fixed_paths_compose_with_sql_and_preserve_multiplicity() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    let out = q(
        &c,
        "SELECT g.node, p.name, g.edge, e.weight, g.depth, g.root
         FROM GRAPH(people:alice -> follows -> people) AS g
         JOIN people AS p ON p.id = g.node
         JOIN follows AS e ON e.id = g.edge
         ORDER BY p.name",
    );
    assert_eq!(
        out.rows,
        vec![
            vec![
                rec("people", "bob"),
                Value::String("Bob".into()),
                rec("follows", "ab"),
                Value::Integer(5),
                Value::Integer(1),
                rec("people", "alice")
            ],
            vec![
                rec("people", "carol"),
                Value::String("Carol".into()),
                rec("follows", "ac"),
                Value::Integer(1),
                Value::Integer(1),
                rec("people", "alice")
            ],
        ]
    );
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.node FROM GRAPH(people:dave <- follows <- people) AS g ORDER BY g.node"
            )
            .rows
        ),
        vec!["bob", "carol"]
    );
    assert_eq!(
        keys(&q(&c, "SELECT g.node FROM GRAPH(people:alice <-> follows <-> people) AS g ORDER BY g.node").rows),
        vec!["bob", "carol", "dave"]
    );
    let two = q(
        &c,
        "SELECT g.node, g.nodes, g.edges FROM GRAPH(people:alice -> follows -> people -> follows -> people WITH PATH) g ORDER BY g.edge",
    );
    assert_eq!(two.rows.len(), 2);
    assert_eq!(keys(&two.rows), vec!["dave", "dave"]);
    assert_eq!(
        two.rows[0][1],
        Value::Array(vec![
            rec("people", "alice"),
            rec("people", "bob"),
            rec("people", "dave")
        ])
    );
    assert_eq!(
        two.rows[0][2],
        Value::Array(vec![rec("follows", "ab"), rec("follows", "bd")])
    );
    assert_eq!(
        q(&c, "SELECT count(DISTINCT g.node) FROM GRAPH(people:alice -> follows -> people -> follows -> people) g").rows,
        vec![vec![Value::Integer(1)]]
    );
    let mixed = q(
        &c,
        "SELECT a.title, count(*) FROM GRAPH(people:alice -> follows -> people -> wrote -> articles) AS g
         JOIN articles a ON a.id = g.node GROUP BY a.title ORDER BY a.title",
    );
    assert_eq!(
        mixed.rows,
        vec![vec![Value::String("Graphs".into()), Value::Integer(1)]]
    );
    assert!(q(
        &c,
        "SELECT * FROM GRAPH(people:nobody -> follows -> people) g"
    )
    .rows
    .is_empty());
    assert!(q(
        &c,
        "SELECT g.node FROM GRAPH(articles:a1 <- wrote <- people -> follows -> people) g WHERE 0"
    )
    .rows
    .is_empty());
    for (sql, code) in [
        (
            "SELECT * FROM GRAPH(people:alice -> wrote -> people) g",
            "FDB_VALIDATION",
        ),
        (
            "SELECT * FROM GRAPH(people:alice <- wrote <- articles) g",
            "FDB_VALIDATION",
        ),
        (
            "SELECT * FROM GRAPH(people:alice -> people -> people) g",
            "FDB_VALIDATION",
        ),
        (
            "SELECT * FROM GRAPH(nothing:x -> follows -> people) g",
            "FDB_NOT_FOUND",
        ),
        (
            "SELECT * FROM GRAPH($missing -> follows -> people) g",
            "FDB_PARAMETER",
        ),
        (
            "SELECT * FROM GRAPH(people:alice -> follows) g",
            "FDB_SYNTAX",
        ),
    ] {
        assert_eq!(fails(&c, sql).code(), code, "{sql}");
    }
    let mut params = Parameters::new();
    params.insert("$root".into(), Value::Null);
    assert_eq!(
        c.execute("SELECT * FROM GRAPH($root -> follows -> people) g", &params)
            .unwrap_err()
            .code(),
        "FDB_VALIDATION"
    );
    assert_eq!(
        q(&c, r#"SELECT '{"a":2}' -> '$.a', '{"a":2}' ->> '$.a'"#).rows,
        vec![vec![Value::String("2".into()), Value::Integer(2)]]
    );
    q(&c, "CREATE TABLE graph (n INTEGER)");
    q(&c, "INSERT INTO graph VALUES (1)");
    assert_eq!(
        q(&c, "SELECT graph.n FROM graph").rows,
        vec![vec![Value::Integer(1)]]
    );
}

#[test]
fn self_loops_and_parallel_edges_follow_their_rules() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    for sql in [
        "CREATE TABLE n",
        "CREATE EDGE TABLE e FROM n TO n ON DELETE CASCADE",
        "INSERT INTO n:a {}",
        "INSERT INTO n:b {}",
        "RELATE n:a -> e:loop -> n:a",
        "RELATE n:a -> e:p1 -> n:b",
        "RELATE n:a -> e:p2 -> n:b",
    ] {
        q(&c, sql);
    }
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.edge FROM GRAPH(n:a <-> e <-> n) g ORDER BY g.edge"
            )
            .rows
        ),
        vec!["loop", "p1", "p2"]
    );
    assert_eq!(
        q(&c, "SELECT count(*) FROM GRAPH(n:a -> e -> n -> e -> n) g").rows,
        vec![vec![Value::Integer(3)]]
    );
    let paths = q(
        &c,
        "SELECT g.node, g.edge FROM GRAPH(n:a -> e -> n DEPTH 1 TO 3 MODE PATHS) g ORDER BY g.edge",
    );
    assert_eq!(keys(&paths.rows), vec!["b", "b"]);
    let reachable = q(
        &c,
        "SELECT g.node, g.depth FROM GRAPH(n:a -> e -> n DEPTH 0 TO 3 MODE REACHABLE) g ORDER BY g.depth",
    );
    assert_eq!(keys(&reachable.rows), vec!["a", "b"]);
    q(&c, "DELETE FROM n WHERE id = n:a");
    assert_eq!(
        q(&c, "SELECT count(*) FROM e").rows[0][0],
        Value::Integer(0)
    );
    c.check_collection_integrity("e", Default::default())
        .unwrap();
}

#[test]
fn recursive_modes_follow_shortest_distance_and_simple_path_rules() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    // a -> b -> c -> d, plus shortcut a -> d and cycle d -> a, and a diamond via e.
    for sql in [
        "CREATE TABLE n",
        "CREATE EDGE TABLE e FROM n TO n",
        "INSERT INTO n:a {}",
        "INSERT INTO n:b {}",
        "INSERT INTO n:c {}",
        "INSERT INTO n:d {}",
        "INSERT INTO n:x {}",
        "RELATE n:a -> e:ab -> n:b",
        "RELATE n:b -> e:bc -> n:c",
        "RELATE n:c -> e:cd -> n:d",
        "RELATE n:a -> e:ad -> n:d",
        "RELATE n:d -> e:da -> n:a",
        "RELATE n:a -> e:ax -> n:x",
        "RELATE n:x -> e:xc -> n:c",
    ] {
        q(&c, sql);
    }
    let reachable = q(
        &c,
        "SELECT g.node, g.depth, g.edges FROM GRAPH(n:a -> e -> n DEPTH 1 TO 4 MODE REACHABLE WITH PATH) g ORDER BY g.depth, g.node",
    );
    assert_eq!(keys(&reachable.rows), vec!["b", "d", "x", "c"]);
    assert_eq!(reachable.rows[3][1], Value::Integer(2));
    assert_eq!(
        reachable.rows[3][2],
        Value::Array(vec![rec("e", "ab"), rec("e", "bc")])
    );
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.node FROM GRAPH(n:a -> e -> n DEPTH 2 TO 3 MODE REACHABLE) g"
            )
            .rows
        ),
        vec!["c"]
    );
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.node FROM GRAPH(n:a -> e -> n DEPTH 0 TO 0 MODE REACHABLE) g"
            )
            .rows
        ),
        vec!["a"]
    );
    let root = q(
        &c,
        "SELECT g.edge, g.nodes, g.edges FROM GRAPH(n:a -> e -> n DEPTH 0 TO 0 MODE PATHS) g",
    );
    assert_eq!(
        root.rows,
        vec![vec![
            Value::Null,
            Value::Array(vec![rec("n", "a")]),
            Value::Array(vec![])
        ]]
    );
    let paths = q(
        &c,
        "SELECT g.node, g.depth FROM GRAPH(n:a -> e -> n DEPTH 1 TO 4 MODE PATHS) g WHERE g.node = n:d ORDER BY g.depth",
    );
    assert_eq!(
        paths.rows.iter().map(|r| r[1].clone()).collect::<Vec<_>>(),
        vec![Value::Integer(1), Value::Integer(3), Value::Integer(3)]
    );
    assert_eq!(
        q(
            &c,
            "SELECT count(*) FROM GRAPH(n:a <- e <- n DEPTH 1 TO 4 MODE REACHABLE) g"
        )
        .rows,
        vec![vec![Value::Integer(4)]]
    );
    assert_eq!(
        fails(
            &c,
            "SELECT * FROM GRAPH(n:a -> e -> n DEPTH 1 TO 33 MODE REACHABLE) g"
        )
        .code(),
        "FDB_LIMIT"
    );
    q(&c, "CREATE TABLE m");
    q(&c, "CREATE EDGE TABLE nm FROM n TO m");
    assert_eq!(
        fails(
            &c,
            "SELECT * FROM GRAPH(n:a -> nm -> m DEPTH 1 TO 1 MODE REACHABLE) g"
        )
        .code(),
        "FDB_VALIDATION"
    );
}

#[test]
fn step_predicates_prune_expansion_while_outer_filters_only_remove_rows() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    let mut params = Parameters::new();
    params.insert("$min".into(), Value::Integer(3));
    let strong = c
        .execute(
            "SELECT g.node FROM GRAPH(people:alice -> (follows AS f WHERE f.weight >= $min) -> people DEPTH 1 TO 3 MODE REACHABLE) g ORDER BY g.node",
            &params,
        )
        .unwrap();
    assert_eq!(keys(&strong.rows), vec!["bob", "dave"]);
    let active = q(
        &c,
        "SELECT g.node FROM GRAPH(people:alice -> follows -> (people AS p WHERE p.active = true) DEPTH 1 TO 3 MODE REACHABLE) g ORDER BY g.node",
    );
    assert_eq!(keys(&active.rows), vec!["bob", "dave"]);
    let outer = q(
        &c,
        "SELECT g.node FROM GRAPH(people:alice -> follows -> people DEPTH 1 TO 3 MODE REACHABLE) g
         JOIN people p ON p.id = g.node WHERE p.active = true ORDER BY g.node",
    );
    assert_eq!(keys(&outer.rows), vec!["bob", "dave"]);
    let missing = q(
        &c,
        "SELECT g.node FROM GRAPH(people:alice -> (follows WHERE missing IS NULL AND NOT (weight < 2)) -> people) g",
    );
    assert_eq!(keys(&missing.rows), vec!["bob"]);
    let error = fails(
        &c,
        "SELECT * FROM GRAPH(people:alice -> (follows WHERE weight + 1 > 2) -> people) g",
    );
    assert_eq!(error.code(), "FDB_UNSUPPORTED", "{error}");
}

#[test]
fn graph_work_is_bounded_explained_and_profiled() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    q(&c, "CREATE TABLE n");
    q(&c, "CREATE EDGE TABLE e FROM n TO n");
    q(&c, "BEGIN");
    q(&c, "INSERT INTO n:hub {}");
    for i in 0..200 {
        q(&c, &format!("INSERT INTO n:m{i} {{}}"));
        q(&c, &format!("RELATE n:hub -> e -> n:m{i}"));
        q(&c, &format!("RELATE n:m{i} -> e -> n:hub"));
    }
    q(&c, "COMMIT");
    let profile = c
        .profile_select(
            "SELECT count(*) FROM GRAPH(n:hub -> e -> n DEPTH 1 TO 2 MODE REACHABLE) g",
            &Parameters::new(),
        )
        .unwrap();
    assert_eq!(profile.result.rows, vec![vec![Value::Integer(200)]]);
    assert_eq!(profile.metrics.graph_adjacency_entries, 400);
    assert_eq!(profile.metrics.graph_rows, 200);
    assert_eq!(profile.metrics.graph_visited, 201);
    assert!(profile.metrics.graph_peak_bytes > 0);
    let plan = q(
        &c,
        "EXPLAIN QUERY PLAN SELECT g.node FROM GRAPH(n:hub -> e -> n DEPTH 1 TO 2 MODE REACHABLE) g",
    );
    let details: Vec<String> = plan
        .rows
        .iter()
        .map(|row| match row.last() {
            Some(Value::String(s)) => s.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(
        details
            .iter()
            .any(|d| d.contains("SEARCH USING COVERING INDEX __romedb_g_65_out")),
        "{details:?}"
    );
    assert!(details
        .iter()
        .any(|d| d.starts_with("GRAPH RECURSION DEPTH 1 TO 2 MODE REACHABLE")));
    c.set_graph_limits(GraphLimits {
        max_adjacency_entries: 100,
        ..GraphLimits::default()
    })
    .unwrap();
    let error = fails(
        &c,
        "SELECT count(*) FROM GRAPH(n:hub -> e -> n DEPTH 1 TO 2 MODE REACHABLE) g",
    );
    assert_eq!(error.code(), "FDB_LIMIT");
    assert!(error.to_string().contains("adjacency entries"), "{error}");
    c.set_graph_limits(GraphLimits {
        max_rows: 10,
        ..GraphLimits::default()
    })
    .unwrap();
    assert_eq!(
        fails(&c, "SELECT count(*) FROM GRAPH(n:hub -> e -> n) g").code(),
        "FDB_LIMIT"
    );
    assert_eq!(
        fails(&c, "SELECT g.node FROM GRAPH(n:hub -> e -> n) g LIMIT 1").code(),
        "FDB_LIMIT"
    );
    assert!(c
        .set_graph_limits(GraphLimits {
            max_depth: 64,
            ..GraphLimits::default()
        })
        .is_err());
    c.set_graph_limits(GraphLimits {
        max_path_states: 50,
        ..GraphLimits::default()
    })
    .unwrap();
    assert_eq!(
        fails(
            &c,
            "SELECT count(*) FROM GRAPH(n:hub -> e -> n DEPTH 1 TO 4 MODE PATHS) g"
        )
        .code(),
        "FDB_LIMIT"
    );
}

#[test]
fn traversal_sees_pending_writes_and_relate_rolls_back() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    q(&c, "BEGIN");
    q(&c, "INSERT INTO people:erin { name: 'Erin' }");
    q(&c, "RELATE people:alice -> follows:ae -> people:erin");
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.node FROM GRAPH(people:alice -> follows -> people) g ORDER BY g.node"
            )
            .rows
        ),
        vec!["bob", "carol", "erin"]
    );
    q(&c, "ROLLBACK");
    assert_eq!(
        keys(
            &q(
                &c,
                "SELECT g.node FROM GRAPH(people:alice -> follows -> people) g ORDER BY g.node"
            )
            .rows
        ),
        vec!["bob", "carol"]
    );
    c.check_collection_integrity("follows", Default::default())
        .unwrap();
}

#[test]
fn graph_marker_persists_after_drops_and_rejects_tampering() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marker.db");
    {
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let c = db.connect().unwrap();
        q(&c, "CREATE TABLE n");
        q(&c, "CREATE EDGE TABLE e FROM n TO n");
        q(&c, "DROP TABLE e");
        q(&c, "DROP TABLE n");
    }
    let path = path.to_str().unwrap();
    let raw = turso_core::Database::open_file(
        turso_core::Database::io_for_path(path).unwrap(),
        path,
        std::sync::Arc::new(turso_core::SqliteDialect),
    )
    .unwrap();
    let raw = raw.connect().unwrap();
    let mut statement = raw
        .prepare("SELECT json_extract(metadata,'$.version') FROM __romedb_catalog WHERE name='__romedb_graph'")
        .unwrap();
    let mut versions = Vec::new();
    statement
        .run_with_row_callback(|row| {
            versions.push(row.get_values().next().cloned());
            Ok(())
        })
        .unwrap();
    assert_eq!(versions.len(), 1);
    raw.execute("UPDATE __romedb_catalog SET metadata=json_set(metadata,'$.version',5) WHERE name='__romedb_graph'")
        .unwrap();
    drop(raw);
    let db = Database::open(path).unwrap();
    assert!(db.connect().is_err());
}

#[test]
fn concurrent_endpoint_deletion_never_leaves_a_dangling_edge() {
    for (mvcc, relate_first) in [(false, true), (false, false), (true, true), (true, false)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        let db = Database::open(path.to_str().unwrap()).unwrap();
        let setup = db.connect().unwrap();
        if mvcc {
            q(&setup, "PRAGMA journal_mode=mvcc");
        }
        for sql in [
            "CREATE TABLE people",
            "CREATE EDGE TABLE follows FROM people TO people",
            "INSERT INTO people:a {}",
            "INSERT INTO people:b {}",
        ] {
            q(&setup, sql);
        }
        let begin = if mvcc { "BEGIN CONCURRENT" } else { "BEGIN" };
        let writer = db.connect().unwrap();
        let deleter = db.connect().unwrap();
        let (first, second, first_sql, second_sql) = if relate_first {
            (
                &writer,
                &deleter,
                "RELATE people:a -> follows:ab -> people:b",
                "DELETE FROM people WHERE id = people:b",
            )
        } else {
            (
                &deleter,
                &writer,
                "DELETE FROM people WHERE id = people:b",
                "RELATE people:a -> follows:ab -> people:b",
            )
        };
        q(first, begin);
        q(first, first_sql);
        let second_result = second.execute(second_sql, &Parameters::new());
        let first_commit = first.execute("COMMIT", &Parameters::new());
        if first_commit.is_err() {
            let _ = first.execute("ROLLBACK", &Parameters::new());
        }
        let check = db.connect().unwrap();
        let edges = q(&check, "SELECT count(*) FROM follows").rows[0][0].clone();
        let b = q(&check, "SELECT count(*) FROM people WHERE id = people:b").rows[0][0].clone();
        assert!(
            !(edges == Value::Integer(1) && b == Value::Integer(0)),
            "mvcc={mvcc} relate_first={relate_first}: dangling edge (second: {:?}, commit: {:?})",
            second_result.as_ref().map(|_| ()),
            first_commit.as_ref().map(|_| ())
        );
        assert!(
            second_result.is_err() || first_commit.is_err(),
            "mvcc={mvcc} relate_first={relate_first}: both conflicting writes succeeded"
        );
        check
            .check_collection_integrity("follows", Default::default())
            .unwrap();
    }
}

#[test]
fn graph_sources_are_rejected_in_writes() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    social(&c);
    q(&c, "CREATE TABLE copies (n BLOB)");
    for sql in [
        "INSERT INTO copies SELECT g.node FROM GRAPH(people:alice -> follows -> people) g",
        "INSERT INTO people (id) SELECT g.node FROM GRAPH(people:alice -> follows -> people) g",
        "DELETE FROM people WHERE id IN (SELECT g.node FROM GRAPH(people:alice -> follows -> people) g)",
    ] {
        assert_eq!(fails(&c, sql).code(), "FDB_UNSUPPORTED", "{sql}");
    }
    assert_eq!(
        q(&c, "SELECT count(*) FROM people").rows[0][0],
        Value::Integer(4)
    );
}
