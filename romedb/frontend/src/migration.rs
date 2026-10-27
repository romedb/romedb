//! Forward-only migrations with transactional exact-source history.
use crate::{Connection, Error, Parameters, Result, TransactionState};
use serde::Serialize;
#[derive(Clone, Debug)]
pub struct Migration {
    pub version: i64,
    pub name: String,
    pub sql: String,
}
#[derive(Debug, Serialize)]
pub struct MigrationReport {
    pub already_applied: usize,
    pub applied: Vec<i64>,
}
impl Connection {
    /// Supply the complete, strictly version-ordered history and pending scripts.
    /// All pending scripts and history rows commit in one transaction.
    pub fn migrate(&self, migrations: &[Migration]) -> Result<MigrationReport> {
        if self.transaction_state() != TransactionState::Autocommit {
            return Err(Error::Validation(
                "migration runner requires autocommit".into(),
            ));
        }
        if migrations.len() > 1000
            || migrations.iter().map(|m| m.sql.len()).sum::<usize>() > 16 * 1024 * 1024
        {
            return Err(Error::Limit(
                "migration plan exceeds 1000 entries or 16 MiB".into(),
            ));
        }
        let mut previous = 0;
        let mut scripts = Vec::new();
        for migration in migrations {
            if migration.version <= previous
                || migration.name.is_empty()
                || migration.name.len() > 255
                || migration.name.contains('\0')
            {
                return Err(Error::Validation("migration versions must be positive and strictly increasing; names require 1–255 bytes".into()));
            }
            previous = migration.version;
            if migration.sql.len() > 4 * 1024 * 1024 {
                return Err(Error::Limit("migration script exceeds 4 MiB".into()));
            }
            let statements = romeql_parser::split_script(&migration.sql).map_err(|mut error| {
                error.message = format!("migration {}: {}", migration.version, error.message);
                error
            })?;
            for statement in &statements {
                let tokens = romeql_parser::tokenize(statement.sql).map_err(|mut error| {
                    error.offset += statement.offset;
                    error.message = format!("migration {}: {}", migration.version, error.message);
                    error
                })?;
                let first = tokens.first().expect("nonempty statement");
                if first.kind != romeql_parser::Kind::Word
                    || !matches!(
                        first.text.to_ascii_uppercase().as_str(),
                        "CREATE"
                            | "ALTER"
                            | "DROP"
                            | "INSERT"
                            | "UPDATE"
                            | "DELETE"
                            | "UPSERT"
                            | "DEFINE"
                            | "REMOVE"
                            | "SELECT"
                            | "WITH"
                            | "REINDEX"
                            | "ANALYZE"
                    )
                {
                    return Err(Error::Validation(format!("migration {} at byte {}: statement is not eligible for managed migration transactions",migration.version,statement.offset)));
                }
                if first.text.eq_ignore_ascii_case("CREATE")
                    && tokens.get(1).is_some_and(|t| {
                        t.text.eq_ignore_ascii_case("TEMP")
                            || t.text.eq_ignore_ascii_case("TEMPORARY")
                    })
                {
                    return Err(Error::Validation(
                        "temporary objects are not migration history".into(),
                    ));
                }
            }
            scripts.push(statements);
        }
        self.atomic(|| {
            self.run("CREATE TABLE IF NOT EXISTS __romedb_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, script TEXT NOT NULL)",&[])?;
            self.schema_object("__romedb_migrations", "table", "__romedb_migrations", "CREATE TABLE IF NOT EXISTS __romedb_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, script TEXT NOT NULL)")?;
            self.managed_dependencies("__romedb_migrations", None)?;
            // One extra row proves that the supplied plan omits history; no
            // later ledger rows are needed for validation or execution.
            let sizes=self.run(&format!("SELECT typeof(name),length(CAST(name AS BLOB)),typeof(script),length(CAST(script AS BLOB)) FROM __romedb_migrations ORDER BY version LIMIT {}", migrations.len() + 1),&[])?;
            if sizes.len()>migrations.len() {return Err(Error::Validation("migration plan omits applied history".into()));}
            let mut history_bytes = 0i64;
            for row in sizes {
                let values = row.into_iter().map(crate::from_engine).collect::<Vec<_>>();
                let [crate::Value::String(name_type), crate::Value::Integer(name_bytes), crate::Value::String(script_type), crate::Value::Integer(script_bytes)] = values.as_slice() else {
                    return Err(Error::Storage("invalid migration history value metadata".into()));
                };
                if name_type != "text" || script_type != "text" || *name_bytes < 1 || *script_bytes < 0 {
                    return Err(Error::Storage("invalid migration history value types or lengths".into()));
                }
                if *name_bytes > 255 || *script_bytes > 4 * 1024 * 1024 {
                    return Err(Error::Limit("migration history value exceeds runner limits".into()));
                }
                history_bytes += script_bytes;
                if history_bytes > 16 * 1024 * 1024 {
                    return Err(Error::Limit("migration history SQL exceeds 16 MiB".into()));
                }
            }
            let history=self.run(&format!("SELECT version,name,script FROM __romedb_migrations ORDER BY version LIMIT {}", migrations.len() + 1),&[])?;
            for (row,migration) in history.iter().zip(migrations) {
                let difference = if row.len() != 3 {
                    Some("invalid history row")
                } else if crate::from_engine(row[0].clone()) != crate::Value::Integer(migration.version) {
                    Some("version does not match the applied sequence")
                } else if crate::from_engine(row[1].clone()) != crate::Value::String(migration.name.clone()) {
                    Some("name differs")
                } else if crate::from_engine(row[2].clone()) != crate::Value::String(migration.sql.clone()) {
                    Some("SQL source differs (including whitespace and comments)")
                } else {
                    None
                };
                if let Some(difference) = difference {
                    return Err(Error::Validation(format!("applied migration history differs at supplied version {}: {difference}",migration.version)));
                }
            }
            let mut applied=Vec::new();
            for (migration,statements) in migrations.iter().zip(&scripts).skip(history.len()) {
                for statement in statements {
                    self.execute(statement.sql,&Parameters::new()).map_err(|source|Error::Migration {
                        version:migration.version,offset:statement.offset,source:Box::new(source)
                    })?;
                }
                self.run("INSERT INTO __romedb_migrations (version,name,script) VALUES (?1,?2,?3)",
                    &[crate::EngineValue::from_i64(migration.version),crate::text(&migration.name),crate::text(&migration.sql)])?;
                applied.push(migration.version);
            }
            Ok(MigrationReport {already_applied:history.len(),applied})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incompatible_history_schema_rejects_before_pending_migrations() {
        for extra in 0..3 {
            let db = crate::Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            c.run(if extra != 0 {
                "CREATE TABLE __romedb_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, script TEXT NOT NULL)"
            } else {
                "CREATE TABLE __romedb_migrations (version INTEGER, name TEXT NOT NULL, script TEXT NOT NULL)"
            }, &[]).unwrap();
            if extra == 1 {
                c.run(
                    "CREATE INDEX unexpected_history_index ON __romedb_migrations(name)",
                    &[],
                )
                .unwrap();
            }
            if extra == 2 {
                c.run("CREATE TRIGGER unexpected_history_trigger AFTER INSERT ON __romedb_migrations BEGIN SELECT 1; END", &[]).unwrap();
            }
            let plan = [Migration {
                version: 1,
                name: "first.sql".into(),
                sql: "CREATE TABLE docs;".into(),
            }];
            let error = c.migrate(&plan).unwrap_err();
            assert_eq!(error.code(), "FDB_STORAGE", "{error:?}");
            assert_eq!(c.transaction_state(), TransactionState::Autocommit);
            assert!(c.execute("SELECT * FROM docs", &Parameters::new()).is_err());
            assert!(c
                .run("SELECT * FROM __romedb_migrations", &[])
                .unwrap()
                .is_empty());
            // Simulate external repair solely in this private test fixture.
            c.run("DROP TABLE __romedb_migrations", &[]).unwrap();
            assert_eq!(c.migrate(&plan).unwrap().applied, vec![1]);
            assert_eq!(c.migrate(&plan).unwrap().already_applied, 1);
        }
    }
    #[test]
    fn unexpected_history_trigger_preserves_applied_prefix_and_retry() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let plan = [
            Migration { version: 1, name: "base.sql".into(), sql: "CREATE TABLE docs; CREATE UNIQUE INDEX docs_n ON docs(n); INSERT INTO docs {id:docs:first,n:1}; CREATE TABLE audit(n INTEGER); INSERT INTO audit VALUES(1);".into() },
            Migration { version: 2, name: "pending.sql".into(), sql: "INSERT INTO docs {id:docs:second,n:2};".into() },
        ];
        c.migrate(&plan[..1]).unwrap();
        let history = c
            .run("SELECT version,name,script FROM __romedb_migrations", &[])
            .unwrap();
        // An externally added trigger must never run through the managed runner.
        c.run("CREATE TRIGGER unexpected_history_trigger AFTER INSERT ON __romedb_migrations BEGIN DELETE FROM audit; END", &[]).unwrap();
        assert_eq!(c.migrate(&plan).unwrap_err().code(), "FDB_STORAGE");
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
        assert_eq!(
            c.run("SELECT version,name,script FROM __romedb_migrations", &[])
                .unwrap(),
            history
        );
        assert_eq!(
            c.execute("SELECT n FROM docs", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![crate::Value::Integer(1)]]
        );
        assert_eq!(
            c.execute("SELECT n FROM audit", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![crate::Value::Integer(1)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        c.run("DROP TRIGGER unexpected_history_trigger", &[])
            .unwrap();
        let report = c.migrate(&plan).unwrap();
        assert_eq!(report.already_applied, 1);
        assert_eq!(report.applied, vec![2]);
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &Parameters::new())
                .unwrap()
                .rows,
            vec![
                vec![crate::Value::Integer(1)],
                vec![crate::Value::Integer(2)]
            ]
        );
        assert_eq!(
            c.execute("SELECT n FROM audit", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![crate::Value::Integer(1)]]
        );
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!(c.migrate(&plan).unwrap().already_applied, 2);
    }
    #[test]
    fn excess_history_rejects_short_plan_before_pending_work() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.migrate(&[]).unwrap();
        let rows = (1..=1100)
            .map(|n| format!("({n},'external','')"))
            .collect::<Vec<_>>()
            .join(",");
        c.run(
            &format!("INSERT INTO __romedb_migrations VALUES {rows}"),
            &[],
        )
        .unwrap();
        let plan = [Migration {
            version: 1,
            name: "external".into(),
            sql: "CREATE TABLE docs;".into(),
        }];
        let error = c.migrate(&plan).unwrap_err();
        assert_eq!(error.code(), "FDB_VALIDATION");
        assert!(error.to_string().contains("omits applied history"));
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
        assert!(c.execute("SELECT * FROM docs", &Parameters::new()).is_err());
        assert_eq!(
            c.run("SELECT count(*) FROM __romedb_migrations", &[])
                .unwrap()[0][0],
            crate::EngineValue::from_i64(1100)
        );
    }
    #[test]
    fn oversized_history_values_reject_before_text_materialization() {
        for (name, script, code) in [
            ("é".repeat(128), "".to_owned(), "FDB_LIMIT"),
            (
                "valid".to_owned(),
                "x".repeat(4 * 1024 * 1024 + 1),
                "FDB_LIMIT",
            ),
            ("".to_owned(), "".to_owned(), "FDB_STORAGE"),
        ] {
            let db = crate::Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            c.migrate(&[]).unwrap();
            c.run(
                "INSERT INTO __romedb_migrations VALUES(1,?1,?2)",
                &[crate::text(&name), crate::text(&script)],
            )
            .unwrap();
            let plan = [Migration {
                version: 1,
                name: "valid".into(),
                sql: "CREATE TABLE docs;".into(),
            }];
            assert_eq!(c.migrate(&plan).unwrap_err().code(), code);
            assert_eq!(c.transaction_state(), TransactionState::Autocommit);
            assert!(c.execute("SELECT * FROM docs", &Parameters::new()).is_err());
            c.run("DELETE FROM __romedb_migrations", &[]).unwrap();
            assert_eq!(c.migrate(&plan).unwrap().applied, vec![1]);
        }
    }
    #[test]
    fn aggregate_history_size_and_blob_values_are_rejected() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.migrate(&[]).unwrap();
        let script = crate::text(&"x".repeat(4 * 1024 * 1024));
        let plan = (1..=5)
            .map(|version| Migration {
                version,
                name: "valid".into(),
                sql: "".into(),
            })
            .collect::<Vec<_>>();
        for migration in &plan {
            c.run(
                "INSERT INTO __romedb_migrations VALUES(?1,'valid',?2)",
                &[
                    crate::EngineValue::from_i64(migration.version),
                    script.clone(),
                ],
            )
            .unwrap();
        }
        let error = c.migrate(&plan).unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT");
        assert!(error.to_string().contains("history SQL exceeds 16 MiB"));
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
        c.run("DELETE FROM __romedb_migrations WHERE version>1", &[])
            .unwrap();
        c.run("UPDATE __romedb_migrations SET script=x'00'", &[])
            .unwrap();
        assert_eq!(c.migrate(&plan[..1]).unwrap_err().code(), "FDB_STORAGE");
        assert_eq!(c.transaction_state(), TransactionState::Autocommit);
    }
}
