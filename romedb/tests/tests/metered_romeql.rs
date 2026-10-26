use romedb::{Database, Parameters, ResultLimits, Value, WriteWorkLimits};

#[test]
fn metered_record_updates_normalize_romeql_before_result_limit_validation() {
    for mvcc in [false, true] {
        let db = Database::open(":memory:").unwrap();
        let connection = db
            .connect()
            .unwrap()
            .with_write_buffer_limits(ResultLimits {
                max_rows: 100,
                max_payload_bytes: 16 * 1024,
            });
        let params = Parameters::new();
        if mvcc {
            connection
                .execute("PRAGMA journal_mode=mvcc", &params)
                .unwrap();
        }
        for sql in [
            "CREATE TABLE docs",
            "DEFINE FIELD title ON docs TYPE string REQUIRED",
            "CREATE UNIQUE INDEX title_unique ON docs(title)",
            "INSERT INTO docs {id:docs:a,title:'original'}",
            "INSERT INTO docs {id:docs:b,title:'other'}",
        ] {
            connection.execute(sql, &params).unwrap();
        }
        let limits = ResultLimits {
            max_rows: 100,
            max_payload_bytes: 16 * 1024,
        };
        let work = WriteWorkLimits {
            max_rows_read: Some(100),
            max_row_mutations: Some(10),
            max_vm_steps: Some(100_000),
        };
        let updated =
            connection.write_metered("UPDATE docs:a SET title='changed'", &params, limits, work);
        assert_eq!(updated.outcome.unwrap().affected, 1);
        assert_eq!(updated.work.row_mutations, 1);
        let rejected =
            connection.write_metered("UPDATE docs:a SET title='other'", &params, limits, work);
        assert_eq!(rejected.outcome.unwrap_err().code(), "FDB_CONSTRAINT");
        let limited = connection.write_metered(
            "UPDATE docs:a SET title='limited'",
            &params,
            limits,
            WriteWorkLimits {
                max_row_mutations: Some(0),
                ..work
            },
        );
        assert_eq!(limited.outcome.unwrap_err().code(), "FDB_CANCELLED");
        assert!(limited.work.mutation_budget_exhausted);
        assert_eq!(
            connection
                .execute("SELECT title FROM docs WHERE id=docs:a", &params)
                .unwrap()
                .rows,
            vec![vec![Value::String("changed".into())]]
        );
        connection
            .check_collection_integrity("docs", romedb::IntegrityLimits::default())
            .unwrap();
    }
}
