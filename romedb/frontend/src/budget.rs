use crate::{Error, Key, Result, Value};

/// Explicit row and payload limits for returned query results. Enforcement
/// timing depends on the operation API. This does not bound engine working
/// memory, decoding allocations, or allocator overhead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultLimits {
    pub max_rows: usize,
    /// Column names and string/object-key/record-name bytes use UTF-8 lengths.
    /// Null and booleans cost one byte; numbers and integer keys cost eight.
    /// Binary/vector bytes count verbatim; arrays and objects sum their contents.
    pub max_payload_bytes: usize,
}

pub(crate) struct ResultBudget {
    limits: Option<ResultLimits>,
    rows: usize,
    bytes: usize,
}
impl ResultBudget {
    pub(crate) fn new(limits: Option<ResultLimits>, columns: &[String]) -> Result<Self> {
        let mut budget = Self {
            limits,
            rows: 0,
            bytes: 0,
        };
        if limits.is_some() {
            for column in columns {
                budget.add(column.len())?;
            }
        }
        Ok(budget)
    }
    fn add(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(payload_error)?;
        if self.bytes > self.limits.expect("bounded accounting").max_payload_bytes {
            return Err(payload_error());
        }
        Ok(())
    }
    pub(crate) fn value(&mut self, value: &Value) -> Result<()> {
        if self.limits.is_none() {
            return Ok(());
        }
        match value {
            Value::Null | Value::Boolean(_) => self.add(1),
            Value::Integer(_) | Value::Number(_) => self.add(8),
            Value::String(s) => self.add(s.len()),
            Value::Binary(b) | Value::Vector(b) => self.add(b.len()),
            Value::Record(r) => {
                self.add(r.table.len())?;
                self.add(match &r.key {
                    Key::String(s) => s.len(),
                    Key::Integer(_) => 8,
                })
            }
            Value::Array(values) => {
                for value in values {
                    self.value(value)?;
                }
                Ok(())
            }
            Value::Object(fields) => {
                for (key, value) in fields {
                    self.add(key.len())?;
                    self.value(value)?;
                }
                Ok(())
            }
        }
    }
    pub(crate) fn document(&mut self, document: &crate::Document) -> Result<()> {
        self.row(&[])?;
        self.document_fields(document)
    }
    pub(crate) fn document_fields(&mut self, document: &crate::Document) -> Result<()> {
        if self.limits.is_some() {
            for (key, value) in document {
                self.add(key.len())?;
                self.value(value)?;
            }
        }
        Ok(())
    }
    pub(crate) fn row(&mut self, row: &[Value]) -> Result<()> {
        self.row_with_fetches(row, &[])
    }
    // FETCH slots are charged when resolved, before duplicate expansion.
    pub(crate) fn row_with_fetches(&mut self, row: &[Value], fetched: &[bool]) -> Result<()> {
        let Some(limits) = self.limits else {
            return Ok(());
        };
        if self.rows >= limits.max_rows {
            return Err(Error::Limit("result row limit exceeded".into()));
        }
        self.rows += 1;
        for (i, value) in row.iter().enumerate() {
            if !fetched.get(i).copied().unwrap_or(false) {
                self.value(value)?;
            }
        }
        Ok(())
    }
}
fn payload_error() -> Error {
    Error::Limit("result payload limit exceeded".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_payload_and_checked_overflow() {
        let values = vec![Value::Object(
            [(
                "é".into(),
                Value::Array(vec![
                    Value::Null,
                    Value::Boolean(true),
                    Value::Integer(7),
                    Value::Number(1.5),
                    Value::String("猫".into()),
                    Value::Binary(vec![0, 255]),
                    Value::Record(crate::Record {
                        table: "docs".into(),
                        key: Key::String("é".into()),
                    }),
                    Value::Vector(vec![0; 13]),
                ]),
            )]
            .into(),
        )];
        // Column: 2; object key: 2; array contents: 1+1+8+8+3+2+6+13.
        let limits = ResultLimits {
            max_rows: 1,
            max_payload_bytes: 46,
        };
        ResultBudget::new(Some(limits), &["é".into()])
            .unwrap()
            .row(&values)
            .unwrap();
        assert!(ResultBudget::new(
            Some(ResultLimits {
                max_payload_bytes: 45,
                ..limits
            }),
            &["é".into()]
        )
        .unwrap()
        .row(&values)
        .is_err());
        let mut budget = ResultBudget::new(
            Some(ResultLimits {
                max_rows: usize::MAX,
                max_payload_bytes: usize::MAX,
            }),
            &[],
        )
        .unwrap();
        budget.bytes = usize::MAX;
        assert!(matches!(budget.row(&[Value::Null]), Err(Error::Limit(_))));
    }
}

#[cfg(test)]
mod evaluation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use turso_ext::{scalar, ResultCode, Value as ExtValue};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    #[scalar(name = "result_budget_tick")]
    fn result_budget_tick(args: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        ExtValue::from_integer(args[0].to_integer().unwrap())
    }
    #[test]
    fn result_budget_stops_at_rejected_row_without_planning_callbacks() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"result_budget_tick".as_ptr(),
                1,
                false,
                0,
                result_budget_tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        let p = crate::Parameters::new();
        for sql in [
            "CREATE TABLE native(n INTEGER)",
            "INSERT INTO native VALUES(1),(2),(3)",
            "CREATE TABLE docs",
            "INSERT INTO docs {n:1}",
            "INSERT INTO docs {n:2}",
            "INSERT INTO docs {n:3}",
            "BEGIN",
            "INSERT INTO native VALUES(4)",
        ] {
            c.execute(sql, &p).unwrap();
        }
        for source in ["native", "docs"] {
            for fetch in [false, true] {
                let sql = format!(
                    "SELECT result_budget_tick(n) AS n{} FROM {source}",
                    if fetch {
                        ",record::fetch(missing:a) AS f"
                    } else {
                        ""
                    }
                );
                CALLS.store(0, Ordering::SeqCst);
                c.execute(&format!("EXPLAIN {sql}"), &p).unwrap();
                assert_eq!(CALLS.load(Ordering::SeqCst), 0);
                for accepted in [0, 1, 2] {
                    for rows in [true, false] {
                        let limits = ResultLimits {
                            max_rows: if rows { accepted } else { 100 },
                            max_payload_bytes: if rows {
                                1000
                            } else {
                                1 + usize::from(fetch) + 8 * accepted
                            },
                        };
                        CALLS.store(0, Ordering::SeqCst);
                        let error = c.profile_select_with_limits(&sql, &p, limits).unwrap_err();
                        assert_eq!(error.code(), "FDB_LIMIT");
                        assert_eq!(
                            CALLS.load(Ordering::SeqCst),
                            accepted + 1,
                            "{sql}, {limits:?}"
                        );
                        assert_eq!(c.transaction_state(), crate::TransactionState::Active);
                    }
                }
                CALLS.store(0, Ordering::SeqCst);
                let result = c
                    .select_with_limits(
                        &sql,
                        &p,
                        ResultLimits {
                            max_rows: 4,
                            max_payload_bytes: 1000,
                        },
                    )
                    .unwrap();
                assert_eq!(CALLS.load(Ordering::SeqCst), result.rows.len());
            }
        }
        for write in [
            "UPDATE docs SET n=n+10 RETURNING result_budget_tick(n) AS n",
            "UPDATE docs {n:n+10} RETURNING result_budget_tick(n) AS n",
            "INSERT INTO docs(n) VALUES(4),(5),(6) RETURNING result_budget_tick(n) AS n",
            "DELETE FROM docs RETURNING result_budget_tick(n) AS n",
        ] {
            let before = c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows;
            for accepted in [0, 1, 2] {
                for row_limit in [false, true] {
                    let limits = ResultLimits {
                        max_rows: if row_limit { accepted } else { 3 },
                        max_payload_bytes: if row_limit { 100 } else { 1 + 8 * accepted },
                    };
                    CALLS.store(0, Ordering::SeqCst);
                    assert_eq!(
                        c.write_with_result_limits(write, &p, limits)
                            .unwrap_err()
                            .code(),
                        "FDB_LIMIT"
                    );
                    assert_eq!(
                        CALLS.load(Ordering::SeqCst),
                        accepted + 1,
                        "{write}: {limits:?}"
                    );
                    assert_eq!(c.transaction_state(), crate::TransactionState::Active);
                    assert_eq!(
                        c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
                        before
                    );
                }
            }
        }
        let mixed =
            "INSERT INTO native(n) SELECT n+10 FROM docs RETURNING result_budget_tick(n) AS n";
        CALLS.store(0, Ordering::SeqCst);
        assert_eq!(
            c.write_with_result_limits(
                mixed,
                &p,
                ResultLimits {
                    max_rows: 3,
                    max_payload_bytes: 0
                }
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            0,
            "mixed source metadata rejection precedes RETURNING evaluation"
        );
        assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 4);
        let write = "INSERT INTO native VALUES(5),(6) RETURNING result_budget_tick(n) AS n";
        CALLS.store(0, Ordering::SeqCst);
        let error = c
            .write_with_result_limits(
                write,
                &p,
                ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 0,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT");
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            0,
            "native metadata overflow rejects before mutation or RETURNING evaluation"
        );
        assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 4);
        let error = c
            .write_with_result_limits(
                write,
                &p,
                ResultLimits {
                    max_rows: 1,
                    max_payload_bytes: 17,
                },
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT");
        assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 4);
        assert_eq!(c.transaction_state(), crate::TransactionState::Active);
        assert_eq!(
            c.write_with_result_limits(
                write,
                &p,
                ResultLimits {
                    max_rows: 2,
                    max_payload_bytes: 17
                }
            )
            .unwrap()
            .rows,
            vec![vec![Value::Integer(5)], vec![Value::Integer(6)]]
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert_eq!(c.execute("SELECT n FROM native", &p).unwrap().rows.len(), 3);
        let before = c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows;
        let c = c.with_write_buffer_limits(ResultLimits {
            max_rows: 1,
            max_payload_bytes: 1000,
        });
        CALLS.store(0, Ordering::SeqCst);
        assert_eq!(
            c.execute("UPDATE docs SET n=result_budget_tick(n)+10", &p)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            2,
            "candidate evaluation stops on the first rejected row"
        );
        assert_eq!(
            c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows,
            before
        );
    }
}

impl crate::Connection {
    /// Opt into per-buffer limits for frontend collection-write candidates and
    /// document snapshots, including retained encoded identities for joined UPDATE
    /// duplicate resolution. Each buffer gets its own counters; this is not a total
    /// memory cap and does not constrain native engine write buffers or direct
    /// single-document Rust methods. Ordinary reads keep their existing policy.
    pub fn with_write_buffer_limits(mut self, limits: ResultLimits) -> Self {
        self.write_buffer_limits = Some(limits);
        self
    }
    pub(crate) fn write_buffer_budget(&self) -> Result<ResultBudget> {
        ResultBudget::new(self.write_buffer_limits, &[])
    }
}
