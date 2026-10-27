//! Batched one-hop reads. These execute in the frontend, never in a UDF.
use crate::{
    canonical, from_engine, quote, text, Connection, Document, EngineValue, Error, Key, Record,
    Result, Value,
};
use std::collections::BTreeMap;
const CHUNK: usize = 128;
pub(crate) const MAX_FETCH_REFERENCES: usize = 16_384;
pub(crate) const MAX_FETCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct FetchMetrics {
    pub batches: u64,
    pub rows_read: u64,
    pub vm_steps: u64,
}
impl FetchMetrics {
    pub(crate) fn add(&mut self, statement: &turso_core::Statement) {
        let metrics = statement.metrics();
        self.batches = self.batches.saturating_add(1);
        self.rows_read = self.rows_read.saturating_add(metrics.rows_read);
        self.vm_steps = self.vm_steps.saturating_add(metrics.insn_executed);
    }
}

// Count the logical tagged JSON representation without allocating encoded copies.
pub(crate) struct FetchBudget {
    pub(crate) used: usize,
    pub(crate) limit: usize,
}
impl std::io::Write for FetchBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.used) {
            return Err(std::io::Error::other("fetch byte limit exceeded"));
        }
        self.used += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl FetchBudget {
    pub(crate) fn charge(&mut self, value: &Value) -> Result<usize> {
        let before = self.used;
        serde_json::to_writer(&mut *self, value)
            .map_err(|_| Error::Limit("fetch byte limit exceeded".into()))?;
        Ok(self.used - before)
    }
}
impl Connection {
    /// Resolve typed references in order, preserving duplicates and nulls.
    /// Missing targets become Null. This reads within one transaction snapshot.
    /// At most 16,384 positions and 64 MiB each of fetched/output tagged JSON
    /// values are accepted. Duplicates count toward output; this is not a heap cap.
    pub fn fetch_records(&self, references: &[Value]) -> Result<Vec<Value>> {
        self.fetch_records_profiled(references)
            .map(|(values, _)| values)
    }
    pub(crate) fn fetch_records_profiled(
        &self,
        references: &[Value],
    ) -> Result<(Vec<Value>, FetchMetrics)> {
        self.fetch_records_profiled_with_budget(references, None)
    }
    pub(crate) fn fetch_records_profiled_with_budget(
        &self,
        references: &[Value],
        result_budget: Option<&mut crate::budget::ResultBudget>,
    ) -> Result<(Vec<Value>, FetchMetrics)> {
        if references.len() > MAX_FETCH_REFERENCES {
            return Err(Error::Limit("fetch reference count exceeds 16384".into()));
        }
        let mut metrics = FetchMetrics::default();
        let values = self.atomic(|| {
            self.fetch_records_measured(references, MAX_FETCH_BYTES, &mut metrics, result_budget)
        })?;
        Ok((values, metrics))
    }
    #[cfg(test)]
    fn fetch_records_inner(&self, references: &[Value], max_bytes: usize) -> Result<Vec<Value>> {
        self.fetch_records_measured(references, max_bytes, &mut FetchMetrics::default(), None)
    }
    fn fetch_records_measured(
        &self,
        references: &[Value],
        max_bytes: usize,
        metrics: &mut FetchMetrics,
        mut result_budget: Option<&mut crate::budget::ResultBudget>,
    ) -> Result<Vec<Value>> {
        let mut budget = FetchBudget {
            used: 0,
            limit: max_bytes,
        };
        let mut groups: BTreeMap<String, BTreeMap<Vec<u8>, Record>> = BTreeMap::new();
        let mut identities = Vec::new();
        for value in references {
            let record = match value {
                Value::Null => {
                    identities.push(None);
                    continue;
                }
                Value::Record(record) => Record {
                    table: canonical(&record.table)?,
                    key: record.key.clone(),
                },
                _ => {
                    return Err(Error::Validation(
                        "record::fetch expects a typed record or null".into(),
                    ))
                }
            };
            let encoded = Value::Record(record.clone()).encode()?;
            groups
                .entry(record.table.clone())
                .or_default()
                .insert(encoded.clone(), record);
            identities.push(Some(encoded));
        }
        let mut occurrences = BTreeMap::<Vec<u8>, usize>::new();
        for id in identities.iter().flatten() {
            *occurrences.entry(id.clone()).or_default() += 1;
        }
        let mut found = BTreeMap::new();
        for (table, records) in groups {
            let records = records.into_iter().collect::<Vec<_>>();
            match self.catalog(&table) {
                Ok(collection) => {
                    for chunk in records.chunks(CHUNK) {
                        let slots = (1..=chunk.len())
                            .map(|i| format!("?{i}"))
                            .collect::<Vec<_>>()
                            .join(",");
                        let params = chunk
                            .iter()
                            .map(|(id, _)| EngineValue::Blob(id.clone()))
                            .collect::<Vec<_>>();
                        let mut statement = self.prepare(format!(
                            "SELECT id,doc FROM {} WHERE id IN ({slots})",
                            quote(&collection.storage)
                        ))?;
                        for (i, value) in params.into_iter().enumerate() {
                            statement.bind_at(
                                std::num::NonZeroUsize::new(i + 1).expect("one based"),
                                value,
                            )?;
                        }
                        visit_target_rows(self, &mut statement, |row| {
                            let EngineValue::Blob(id) = &row[0] else {
                                return Err(Error::Storage("invalid fetched id".into()));
                            };
                            let value = Value::Object(crate::decode_document(&row[1])?);
                            let bytes = budget.charge(&value)?;
                            charge_result_target(
                                &mut result_budget,
                                &occurrences,
                                found.contains_key(id),
                                id,
                                &value,
                            )?;
                            found.insert(id.clone(), (value, bytes));
                            Ok(())
                        })?;
                        metrics.add(&statement);
                    }
                }
                Err(Error::NotFound(_)) => {
                    let schema = self.run(
                        "SELECT type FROM main.sqlite_schema WHERE name=?1 COLLATE NOCASE",
                        &[text(&table)],
                    )?;
                    let Some(row) = schema.first() else {
                        continue;
                    };
                    if from_engine(row[0].clone()) != Value::String("table".into()) {
                        return Err(Error::Unsupported(
                            "record::fetch requires a table target".into(),
                        ));
                    }
                    let info =
                        self.run(&format!("PRAGMA main.table_info({})", quote(&table)), &[])?;
                    let keys = info
                        .iter()
                        .filter(|r| matches!(from_engine(r[5].clone()),Value::Integer(i) if i>0))
                        .collect::<Vec<_>>();
                    let [key] = keys.as_slice() else {
                        return Err(Error::Unsupported(
                            "record::fetch requires a single explicit TEXT or INTEGER primary key"
                                .into(),
                        ));
                    };
                    let (Value::String(key_name), Value::String(key_type)) =
                        (from_engine(key[1].clone()), from_engine(key[2].clone()))
                    else {
                        return Err(Error::Storage("invalid primary key metadata".into()));
                    };
                    let integer = match key_type.trim().to_ascii_uppercase().as_str() {
                        "INTEGER" => true,
                        "TEXT" => false,
                        _ => {
                            return Err(Error::Unsupported(
                                "record::fetch requires TEXT or INTEGER primary key".into(),
                            ))
                        }
                    };
                    for (_, record) in &records {
                        if matches!(record.key, Key::Integer(_)) != integer {
                            return Err(Error::Validation(
                                "reference key type differs from target primary key".into(),
                            ));
                        }
                    }
                    for chunk in records.chunks(CHUNK) {
                        let slots = (1..=chunk.len())
                            .map(|i| format!("?{i}"))
                            .collect::<Vec<_>>()
                            .join(",");
                        let sql = format!(
                            "SELECT * FROM main.{} WHERE {} IN ({slots}) AND typeof({})='{}'",
                            quote(&table),
                            quote(&key_name),
                            quote(&key_name),
                            if integer { "integer" } else { "text" }
                        );
                        let mut statement = self.prepare(sql)?;
                        for (i, (_, record)) in chunk.iter().enumerate() {
                            let value = match &record.key {
                                Key::Integer(i) => crate::scalar(&Value::Integer(*i))?,
                                Key::String(s) => text(s),
                            };
                            statement.bind_at(
                                std::num::NonZeroUsize::new(i + 1).expect("one based"),
                                value,
                            )?;
                        }
                        let names = (0..statement.num_columns())
                            .map(|i| statement.get_column_name(i).into_owned())
                            .collect::<Vec<_>>();
                        visit_target_rows(self, &mut statement, |row| {
                            let doc = names
                                .iter()
                                .cloned()
                                .zip(row.into_iter().map(from_engine))
                                .collect::<Document>();
                            let key = match doc.get(&key_name) {
                                Some(Value::Integer(i)) => Key::Integer(*i),
                                Some(Value::String(s)) => Key::String(s.clone()),
                                _ => return Err(Error::Storage("invalid fetched key".into())),
                            };
                            let id = Value::Record(Record {
                                table: table.clone(),
                                key,
                            })
                            .encode()?;
                            let value = Value::Object(doc);
                            let bytes = budget.charge(&value)?;
                            charge_result_target(
                                &mut result_budget,
                                &occurrences,
                                found.contains_key(&id),
                                &id,
                                &value,
                            )?;
                            found.insert(id, (value, bytes));
                            Ok(())
                        })?;
                        metrics.add(&statement);
                    }
                }
                Err(e) => return Err(e),
            }
        }
        // Count duplicate expansion before cloning any output documents.
        let null_bytes = serde_json::to_string(&Value::Null)?.len();
        let mut output_bytes = 0usize;
        for id in &identities {
            let bytes = id
                .as_ref()
                .and_then(|id| found.get(id))
                .map_or(null_bytes, |(_, bytes)| *bytes);
            if bytes > max_bytes.saturating_sub(output_bytes) {
                return Err(Error::Limit("fetch byte limit exceeded".into()));
            }
            output_bytes += bytes;
            // Found targets were charged, with multiplicity, before retention.
            if id.as_ref().and_then(|id| found.get(id)).is_none() {
                if let Some(budget) = result_budget.as_deref_mut() {
                    budget.value(&Value::Null)?;
                }
            }
        }
        Ok(identities
            .into_iter()
            .map(|id| {
                id.and_then(|id| found.get(&id).map(|(value, _)| value.clone()))
                    .unwrap_or(Value::Null)
            })
            .collect())
    }
}

// Native collation can return the same stored identity in different chunks.
// Charge its output occurrences once, using the resolver's exact identity map.
fn charge_result_target(
    budget: &mut Option<&mut crate::budget::ResultBudget>,
    occurrences: &BTreeMap<Vec<u8>, usize>,
    already_found: bool,
    id: &[u8],
    value: &Value,
) -> Result<()> {
    if !already_found {
        if let Some(budget) = budget.as_deref_mut() {
            for _ in 0..occurrences.get(id).copied().unwrap_or(0) {
                budget.value(value)?;
            }
        }
    }
    Ok(())
}

// Preserve frontend budget/decoding errors while interrupting engine iteration.
// The caller drops the statement before its enclosing atomic scope cleans up.
pub(crate) fn visit_target_rows(
    connection: &Connection,
    statement: &mut turso_core::Statement,
    mut visit: impl FnMut(Vec<EngineValue>) -> Result<()>,
) -> Result<()> {
    let mut failure = None;
    let execution = connection.meter_statement(statement, |statement| {
        crate::parser_stack(|| {
            statement
                .run_with_row_callback(|row| {
                    if let Err(error) = visit(row.get_values().cloned().collect()) {
                        failure = Some(error);
                        return Err(turso_core::LimboError::Interrupt);
                    }
                    Ok(())
                })
                .map_err(Error::from)
        })
    });
    if let Err(error @ Error::Rollback { .. }) = execution {
        return Err(error);
    }
    if let Some(error) = failure {
        return Err(error);
    }
    execution?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use turso_ext::{scalar, ResultCode, Value as ExtValue};
    static TARGET_CALLS: AtomicUsize = AtomicUsize::new(0);
    #[scalar(name = "fetch_target_tick")]
    fn target_tick(args: &[ExtValue]) -> ExtValue {
        TARGET_CALLS.fetch_add(1, Ordering::SeqCst);
        ExtValue::from_integer(args[0].to_integer().unwrap())
    }

    #[test]
    fn bounded_fetch_rejects_before_completing_first_target_chunk() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let p = crate::Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE native(id INTEGER PRIMARY KEY,n INTEGER)",
            "BEGIN",
        ] {
            c.execute(sql, &p).unwrap();
        }
        for n in 0..130 {
            c.execute(
                &format!("INSERT INTO docs {{id:type::record('docs',{n}),n:{n}}}"),
                &p,
            )
            .unwrap();
            c.execute(&format!("INSERT INTO native VALUES({n},{n})"), &p)
                .unwrap();
        }
        for table in ["docs", "native"] {
            let refs = (0..130)
                .map(|n| {
                    Value::Record(Record {
                        table: table.into(),
                        key: Key::Integer(n),
                    })
                })
                .collect::<Vec<_>>();
            let mut metrics = FetchMetrics::default();
            let mut budget = crate::budget::ResultBudget::new(
                Some(crate::ResultLimits {
                    max_rows: 130,
                    max_payload_bytes: 0,
                }),
                &[],
            )
            .unwrap();
            let error = c
                .atomic(|| {
                    c.fetch_records_measured(
                        &refs,
                        MAX_FETCH_BYTES,
                        &mut metrics,
                        Some(&mut budget),
                    )
                })
                .unwrap_err();
            assert_eq!(error.code(), "FDB_LIMIT");
            assert_eq!(
                metrics.batches, 0,
                "{table}: failed before completing the first chunk"
            );
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            let mut budget = crate::budget::ResultBudget::new(
                Some(crate::ResultLimits {
                    max_rows: 130,
                    max_payload_bytes: 10000,
                }),
                &[],
            )
            .unwrap();
            let (values, metrics) = c
                .fetch_records_profiled_with_budget(&refs, Some(&mut budget))
                .unwrap();
            assert_eq!(values.len(), 130);
            assert_eq!(metrics.batches, 2);
            assert!(values.iter().all(|v| matches!(v, Value::Object(_))));
        }
        // Distinct reference spellings may return the same stored key in
        // multiple native NOCASE chunks. Its exact output identity is charged once.
        c.execute(
            "CREATE TABLE collated(id TEXT PRIMARY KEY COLLATE NOCASE,n INTEGER)",
            &p,
        )
        .unwrap();
        c.execute("INSERT INTO collated VALUES('abcdefgh',7)", &p)
            .unwrap();
        let refs = (0..130)
            .map(|mask| {
                let key = "abcdefgh"
                    .bytes()
                    .enumerate()
                    .map(|(bit, b)| {
                        if mask & (1 << bit) == 0 {
                            b as char
                        } else {
                            b.to_ascii_uppercase() as char
                        }
                    })
                    .collect::<String>();
                Value::Record(Record {
                    table: "collated".into(),
                    key: Key::String(key),
                })
            })
            .collect::<Vec<_>>();
        let expected = c.fetch_records(&refs).unwrap();
        assert_eq!(
            expected
                .iter()
                .filter(|v| matches!(v, Value::Object(_)))
                .count(),
            1
        );
        // Object fields: id key 2 + string 8 + n key 1 + integer 8; 129 nulls.
        let limits = crate::ResultLimits {
            max_rows: 130,
            max_payload_bytes: 148,
        };
        let mut budget = crate::budget::ResultBudget::new(Some(limits), &[]).unwrap();
        let (actual, metrics) = c
            .fetch_records_profiled_with_budget(&refs, Some(&mut budget))
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(metrics.batches, 2);
        let mut budget = crate::budget::ResultBudget::new(
            Some(crate::ResultLimits {
                max_payload_bytes: 147,
                ..limits
            }),
            &[],
        )
        .unwrap();
        assert_eq!(
            c.fetch_records_profiled_with_budget(&refs, Some(&mut budget))
                .err()
                .unwrap()
                .code(),
            "FDB_LIMIT"
        );
        c.execute("ROLLBACK", &p).unwrap();
        assert!(c.execute("SELECT n FROM docs", &p).unwrap().rows.is_empty());
        assert!(c
            .execute("SELECT n FROM native", &p)
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn interrupted_fetch_profiles_preserve_prior_work_and_retry_counters() {
        use std::sync::{atomic::AtomicBool, Arc};
        for outer in [false, true] {
            let db = crate::Database::open(":memory:").unwrap();
            let c = db.connect().unwrap();
            let params = crate::Parameters::new();
            c.execute("CREATE TABLE docs", &params).unwrap();
            c.execute("CREATE TABLE positions(n INTEGER PRIMARY KEY)", &params)
                .unwrap();
            c.execute("BEGIN", &params).unwrap();
            for n in 0..130 {
                c.execute(
                    &format!("INSERT INTO docs {{id:type::record('docs',{n}),n:{n}}}"),
                    &params,
                )
                .unwrap();
                c.execute(&format!("INSERT INTO positions VALUES({n})"), &params)
                    .unwrap();
            }
            c.execute("COMMIT", &params).unwrap();
            if outer {
                c.execute("BEGIN", &params).unwrap();
                c.execute("INSERT INTO docs {id:docs:prior,n:999}", &params)
                    .unwrap();
            }
            let state = c.transaction_state();
            let sql = "SELECT record::fetch(type::record('docs',n)) FROM positions ORDER BY n";
            let steps = Arc::new(AtomicUsize::new(0));
            let count = steps.clone();
            c.engine.set_progress_handler(
                1,
                Some(Box::new(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    false
                })),
            );
            let baseline = c.profile_select(sql, &params);
            c.engine.set_progress_handler(0, None);
            let baseline = baseline.unwrap();
            assert_eq!(baseline.metrics.fetch_batches, 2);
            let total = steps.load(Ordering::SeqCst);
            assert!(total > 100);
            for stop in [1, total / 4, total / 2, total * 3 / 4, total - 1] {
                let ticks = Arc::new(AtomicUsize::new(0));
                let fired = Arc::new(AtomicBool::new(false));
                let tick = ticks.clone();
                let flag = fired.clone();
                c.engine.set_progress_handler(
                    1,
                    Some(Box::new(move || {
                        tick.fetch_add(1, Ordering::SeqCst) + 1 >= stop
                            && !flag.swap(true, Ordering::SeqCst)
                    })),
                );
                let result = c.profile_select(sql, &params);
                c.engine.set_progress_handler(0, None);
                assert!(fired.load(Ordering::SeqCst), "stop {stop}/{total}");
                assert_eq!(
                    result.unwrap_err().code(),
                    "FDB_CANCELLED",
                    "stop {stop}/{total}, outer={outer}"
                );
                assert_eq!(c.transaction_state(), state);
                let retry = c.profile_select(sql, &params).unwrap();
                assert_eq!(retry.result.rows, baseline.result.rows);
                assert_eq!(retry.metrics, baseline.metrics);
                assert_eq!(
                    c.check_collection_integrity("docs", Default::default())
                        .unwrap()
                        .documents,
                    if outer { 131 } else { 130 }
                );
            }
            if outer {
                c.execute("ROLLBACK", &params).unwrap();
            }
            assert_eq!(
                c.check_collection_integrity("docs", Default::default())
                    .unwrap()
                    .documents,
                130
            );
        }
    }

    #[test]
    fn target_budget_failure_stops_engine_evaluation_before_later_rows() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        unsafe {
            let api = c.engine._build_turso_ext();
            let code = (api.register_scalar_function)(
                api.ctx,
                c"fetch_target_tick".as_ptr(),
                1,
                false,
                0,
                target_tick,
                None,
                None,
            );
            c.engine._free_extension_ctx(api);
            assert_eq!(code, ResultCode::OK);
        }
        let params = crate::Parameters::new();
        c.execute("CREATE TABLE targets(n INTEGER)", &params)
            .unwrap();
        c.execute("BEGIN", &params).unwrap();
        c.execute("INSERT INTO targets VALUES(1),(2),(3)", &params)
            .unwrap();
        for accepted in [0, 1, 2] {
            let bytes = serde_json::to_vec(&Value::Integer(1)).unwrap().len();
            let mut budget = FetchBudget {
                used: 0,
                limit: accepted * bytes,
            };
            TARGET_CALLS.store(0, Ordering::SeqCst);
            let error = c
                .atomic(|| {
                    let mut statement = c.prepare("SELECT fetch_target_tick(n) FROM targets")?;
                    visit_target_rows(&c, &mut statement, |row| {
                        budget.charge(&from_engine(row[0].clone())).map(|_| ())
                    })
                })
                .unwrap_err();
            assert_eq!(error.code(), "FDB_LIMIT");
            assert_eq!(TARGET_CALLS.load(Ordering::SeqCst), accepted + 1);
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            TARGET_CALLS.store(0, Ordering::SeqCst);
            let mut values = Vec::new();
            c.atomic(|| {
                let mut statement = c.prepare("SELECT fetch_target_tick(n) FROM targets")?;
                visit_target_rows(&c, &mut statement, |row| {
                    values.push(from_engine(row[0].clone()));
                    Ok(())
                })
            })
            .unwrap();
            assert_eq!(
                values,
                vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)]
            );
            assert_eq!(TARGET_CALLS.load(Ordering::SeqCst), 3);
        }
        // Every target occurs twice in the eventual output. Reject during
        // target iteration, before retaining the first over-budget target.
        let occurrences = [(vec![1], 2), (vec![2], 2), (vec![3], 2)].into();
        for accepted in [0, 1, 2] {
            let mut result_budget = crate::budget::ResultBudget::new(
                Some(crate::ResultLimits {
                    max_rows: 6,
                    max_payload_bytes: accepted * 16,
                }),
                &[],
            )
            .unwrap();
            let mut found = BTreeMap::new();
            TARGET_CALLS.store(0, Ordering::SeqCst);
            let error = c
                .atomic(|| {
                    let mut statement = c.prepare("SELECT fetch_target_tick(n) FROM targets")?;
                    visit_target_rows(&c, &mut statement, |row| {
                        let value = from_engine(row[0].clone());
                        let Value::Integer(n) = value else {
                            panic!("integer fixture")
                        };
                        let id = vec![n as u8];
                        charge_result_target(
                            &mut Some(&mut result_budget),
                            &occurrences,
                            found.contains_key(&id),
                            &id,
                            &value,
                        )?;
                        found.insert(id, value);
                        Ok(())
                    })
                })
                .unwrap_err();
            assert_eq!(error.code(), "FDB_LIMIT");
            assert_eq!(TARGET_CALLS.load(Ordering::SeqCst), accepted + 1);
            assert_eq!(found.len(), accepted);
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
        }
        for _ in 0..13 {
            c.execute("INSERT INTO targets SELECT n FROM targets", &params)
                .unwrap();
        }
        TARGET_CALLS.store(0, Ordering::SeqCst);
        let error = c
            .execute(
                "SELECT fetch_target_tick(n),record::fetch(type::record('missing',n)) FROM targets",
                &params,
            )
            .unwrap_err();
        assert_eq!(error.code(), "FDB_LIMIT", "{error}");
        assert_eq!(
            TARGET_CALLS.load(Ordering::SeqCst),
            MAX_FETCH_REFERENCES + 1
        );
        assert_eq!(c.transaction_state(), crate::TransactionState::Active);
        TARGET_CALLS.store(0, Ordering::SeqCst);
        let retry = c.execute("SELECT fetch_target_tick(n),record::fetch(type::record('missing',n)) FROM targets LIMIT 2", &params).unwrap();
        assert_eq!(
            retry.rows,
            vec![
                vec![Value::Integer(1), Value::Null],
                vec![Value::Integer(2), Value::Null]
            ]
        );
        assert_eq!(TARGET_CALLS.load(Ordering::SeqCst), 2);
        c.execute("ROLLBACK", &params).unwrap();
        assert!(c
            .execute("SELECT * FROM targets", &params)
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn fetch_byte_limits_include_duplicates_and_preserve_outer_work() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        let params = crate::Parameters::new();
        for sql in [
            "CREATE TABLE docs",
            "CREATE TABLE native(id INTEGER PRIMARY KEY, text TEXT)",
            "BEGIN",
            "INSERT INTO docs {id:docs:a,text:'ไทย'}",
            "INSERT INTO native VALUES(1,'ไทย')",
        ] {
            c.execute(sql, &params).unwrap();
        }
        for reference in [
            Value::Record(Record {
                table: "docs".into(),
                key: Key::String("a".into()),
            }),
            Value::Record(Record {
                table: "native".into(),
                key: Key::Integer(1),
            }),
        ] {
            let references = vec![reference.clone(), reference, Value::Null];
            let expected = c.fetch_records(&references).unwrap();
            let bytes = expected
                .iter()
                .map(|v| serde_json::to_vec(v).unwrap().len())
                .sum();
            assert_eq!(
                c.atomic(|| c.fetch_records_inner(&references, bytes))
                    .unwrap(),
                expected
            );
            for limit in [0, 1, bytes - 1] {
                assert_eq!(
                    c.atomic(|| c.fetch_records_inner(&references, limit))
                        .unwrap_err()
                        .code(),
                    "FDB_LIMIT"
                );
                assert_eq!(c.transaction_state(), crate::TransactionState::Active);
                assert_eq!(c.fetch_records(&references).unwrap(), expected);
            }
        }
        let null_bytes = serde_json::to_vec(&Value::Null).unwrap().len();
        assert_eq!(
            c.fetch_records_inner(&[Value::Null], null_bytes).unwrap(),
            vec![Value::Null]
        );
        assert_eq!(
            c.fetch_records_inner(&[Value::Null], null_bytes - 1)
                .unwrap_err()
                .code(),
            "FDB_LIMIT"
        );
        assert!(c.fetch_records_inner(&[], 0).unwrap().is_empty());
        c.execute("ROLLBACK", &params).unwrap();
        assert!(c
            .execute("SELECT * FROM docs", &params)
            .unwrap()
            .rows
            .is_empty());
        assert!(c
            .execute("SELECT * FROM native", &params)
            .unwrap()
            .rows
            .is_empty());
    }
}
