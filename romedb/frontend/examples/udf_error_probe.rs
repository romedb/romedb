// Current-engine adapter of the historical proposal reproduction.
// Native scalar-error transaction regression. No RomeDB lowering or JavaScript.
use std::sync::Arc;
use turso_core::{Connection, Database, DatabaseOpts, OpenFlags, Value};
use turso_ext::{scalar, Value as ExtValue};
#[scalar(name = "fail_three")]
fn fail_three(args: &[ExtValue]) -> ExtValue {
    let value = args[0].to_integer().unwrap();
    if value == 3 {
        ExtValue::error_with_message("expected scalar failure".into())
    } else {
        ExtValue::from_integer(value + 10)
    }
}
fn run(c: &Arc<Connection>, sql: &str) -> turso_core::Result<Vec<Vec<Value>>> {
    let mut statement = c.prepare(sql)?;
    let mut rows = Vec::new();
    statement.run_with_row_callback(|row| {
        rows.push(row.get_values().cloned().collect());
        Ok(())
    })?;
    Ok(rows)
}
fn main() {
    for (outer, named) in [(false, false), (false, true), (true, true)] {
        for query in [
            "SELECT fail_three(3)",
            "SELECT fail_three(n) FROM docs",
            "UPDATE docs SET n=fail_three(n)",
            "INSERT INTO copied SELECT fail_three(n) FROM docs",
            "UPDATE OR FAIL docs SET n=fail_three(n)",
            "UPDATE OR ROLLBACK docs SET n=fail_three(n)",
        ] {
            let db = Database::open_file_with_flags(
                Database::io_for_path(":memory:").unwrap(),
                ":memory:",
                OpenFlags::default(),
                DatabaseOpts::new(),
                None,
                Arc::new(turso_core::SqliteDialect),
            )
            .unwrap();
            let c = db.connect().unwrap();
            unsafe {
                let api = c._build_turso_ext();
                assert_eq!(
                    (api.register_scalar_function)(
                        api.ctx,
                        c"fail_three".as_ptr(),
                        1,
                        false,
                        0,
                        fail_three,
                        None,
                        None
                    ),
                    turso_ext::ResultCode::OK
                );
                c._free_extension_ctx(api);
            }
            run(&c, "CREATE TABLE docs(n INTEGER UNIQUE)").unwrap();
            run(&c, "CREATE TABLE copied(n INTEGER)").unwrap();
            run(&c, "INSERT INTO docs VALUES(1),(2),(3)").unwrap();
            if outer {
                run(&c, "BEGIN").unwrap();
                run(&c, "INSERT INTO docs VALUES(4)").unwrap();
            }
            if named {
                run(&c, "SAVEPOINT operation").unwrap();
                run(&c, "SAVEPOINT inner_operation").unwrap();
            }
            let before = run(&c, "SELECT n FROM docs ORDER BY n").unwrap();
            let error = run(&c, query).unwrap_err();
            assert!(
                matches!(error, turso_core::LimboError::ExtensionError(_)),
                "{error:?}"
            );
            assert_eq!(
                c.get_auto_commit(),
                !named,
                "scalar error aborted caller transaction: outer={outer}, {query}"
            );
            assert_eq!(
                run(&c, "SELECT n FROM docs ORDER BY n").unwrap(),
                before,
                "partial writes or prior-work loss: {query}"
            );
            assert!(run(&c, "SELECT * FROM copied").unwrap().is_empty());
            if named {
                run(&c, "ROLLBACK TO operation").unwrap();
                run(&c, "RELEASE operation").unwrap();
            }
            assert_eq!(c.get_auto_commit(), !outer);
            if outer {
                run(&c, "COMMIT").unwrap();
            }
            run(&c, "SAVEPOINT next_operation").unwrap();
            run(&c, "RELEASE next_operation").unwrap();
            assert!(c.get_auto_commit(), "savepoint state leaked: {query}");
            assert_eq!(run(&c, "SELECT n FROM docs ORDER BY n").unwrap(), before);
            println!("PASS outer={outer} named={named}: {query}");
        }
    }
}
