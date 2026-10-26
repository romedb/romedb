use napi_derive::napi;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
static NEXT_INTERRUPT: AtomicU64 = AtomicU64::new(1);
static INTERRUPTS: OnceLock<Mutex<BTreeMap<u64, romedb::InterruptHandle>>> = OnceLock::new();
fn interrupts() -> &'static Mutex<BTreeMap<u64, romedb::InterruptHandle>> {
    INTERRUPTS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[napi]
pub fn interrupt_connection(key: String) -> bool {
    let Ok(key) = key.parse::<u64>() else {
        return false;
    };
    let handle = interrupts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    handle.is_some_and(|h| h.interrupt())
}
static NEXT_CANCELLATION: AtomicU64 = AtomicU64::new(1);
static CANCELLATIONS: OnceLock<Mutex<BTreeMap<u64, romedb::CancellationToken>>> = OnceLock::new();
fn cancellations() -> &'static Mutex<BTreeMap<u64, romedb::CancellationToken>> {
    CANCELLATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
#[napi]
pub fn create_cancellation_token(timeout_ms: Option<String>) -> napi::Result<String> {
    let token = if let Some(timeout_ms) = timeout_ms {
        let millis = timeout_ms
            .parse::<u32>()
            .map_err(|_| error("invalid operation timeout"))?;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(u64::from(millis)))
            .ok_or_else(|| error("operation deadline overflow"))?;
        romedb::CancellationToken::with_deadline(deadline)
    } else {
        romedb::CancellationToken::new()
    };
    let mut tokens = cancellations().lock().unwrap_or_else(|e| e.into_inner());
    if tokens.len() >= 16384 {
        return Err(error("cancellation token limit exceeded"));
    }
    let id = NEXT_CANCELLATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| error("cancellation identifiers exhausted"))?;
    tokens.insert(id, token);
    Ok(id.to_string())
}
#[napi]
pub fn cancel_operation(key: String) -> bool {
    let Ok(key) = key.parse::<u64>() else {
        return false;
    };
    let token = cancellations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    if let Some(token) = token {
        token.cancel();
        true
    } else {
        false
    }
}
#[napi]
pub fn release_cancellation_token(key: String) {
    if let Ok(key) = key.parse::<u64>() {
        cancellations()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
}
fn error(error: impl std::fmt::Display) -> napi::Error {
    napi::Error::from_reason(error.to_string())
}
/// Standalone typed construction; no database connection is opened.
#[napi]
pub fn vector_from_components(
    encoding: String,
    components: napi::bindgen_prelude::Buffer,
) -> napi::Result<napi::bindgen_prelude::Buffer> {
    if components.is_empty() || components.len() % 8 != 0 || components.len() > 65_536 * 8 {
        return Err(error("vector components require 1..65536 binary64 values"));
    }
    let values = components
        .chunks_exact(8)
        .map(|bytes| f64::from_le_bytes(bytes.try_into().expect("binary64 width")))
        .collect::<Vec<_>>();
    let value = if encoding == "float64" {
        romedb::Value::vector64(&values)
    } else {
        let values = values.iter().map(|value| *value as f32).collect::<Vec<_>>();
        match encoding.as_str() {
            "float32" => romedb::Value::vector32(&values),
            "sparse32" => romedb::Value::vector32_sparse(&values),
            "quantized8" => romedb::Value::vector8(&values),
            "bit1" => romedb::Value::vector1bit(&values),
            _ => return Err(error("unknown vector encoding")),
        }
    }
    .map_err(error)?;
    let romedb::Value::Vector(bytes) = value else {
        unreachable!("typed vector constructor")
    };
    Ok(bytes.into())
}
#[napi]
pub fn vector_from_sparse_entries(
    dimensions: f64,
    entries: napi::bindgen_prelude::Buffer,
) -> napi::Result<napi::bindgen_prelude::Buffer> {
    if !dimensions.is_finite()
        || dimensions.fract() != 0.0
        || !(1.0..=65_536.0).contains(&dimensions)
    {
        return Err(error("vector dimensions must be 1..65536"));
    }
    let dimensions = dimensions as usize;
    if entries.len() % 12 != 0 || entries.len() / 12 > dimensions {
        return Err(error("sparse entries require bounded index/binary64 pairs"));
    }
    let entries = entries
        .chunks_exact(12)
        .map(|bytes| {
            let index = u32::from_le_bytes(bytes[..4].try_into().expect("index width"));
            let value = f64::from_le_bytes(bytes[4..].try_into().expect("binary64 width"));
            (index as usize, value as f32)
        })
        .collect::<Vec<_>>();
    let romedb::Value::Vector(bytes) =
        romedb::Value::vector32_sparse_entries(dimensions, &entries).map_err(error)?
    else {
        unreachable!("typed vector constructor")
    };
    Ok(bytes.into())
}
#[napi]
pub struct NativeDatabase {
    interrupt_key: u64,
    inner: Option<(romedb::Connection, romedb::Database)>,
}
#[napi]
impl NativeDatabase {
    #[napi(constructor)]
    pub fn new(
        path: String,
        max_rows: Option<String>,
        max_payload_bytes: Option<String>,
    ) -> napi::Result<Self> {
        let limits = match (max_rows, max_payload_bytes) {
            (None, None) => None,
            (Some(rows), Some(bytes)) => Some(romedb::ResultLimits {
                max_rows: rows
                    .parse()
                    .map_err(|_| error("invalid write buffer row limit"))?,
                max_payload_bytes: bytes
                    .parse()
                    .map_err(|_| error("invalid write buffer payload limit"))?,
            }),
            _ => return Err(error("both write buffer limits are required")),
        };
        let db = romedb::Database::open(&path).map_err(error)?;
        let conn = db.connect().map_err(error)?;
        let conn = if let Some(limits) = limits {
            conn.with_write_buffer_limits(limits)
        } else {
            conn
        };
        let interrupt_key = NEXT_INTERRUPT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| error("interrupt identifiers exhausted"))?;
        interrupts()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(interrupt_key, conn.interrupt_handle());
        Ok(Self {
            interrupt_key,
            inner: Some((conn, db)),
        })
    }
    #[napi]
    pub fn close(&mut self) {
        self.close_inner();
    }
    #[napi]
    pub fn interrupt_key(&self) -> String {
        self.interrupt_key.to_string()
    }
    #[napi]
    pub fn execute(&self, sql: String, parameters: String) -> napi::Result<String> {
        self.report(|conn| {
            let parameters = decode_parameters(&parameters)?;
            query_value(conn.execute(&sql, &parameters)?)
        })
    }
    #[napi]
    pub fn execute_atomic_request(
        &self,
        body: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = match cancellation_key {
            Some(key) => cancellations()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key.parse::<u64>().map_err(error)?)
                .cloned()
                .ok_or_else(|| error("unknown cancellation token"))?,
            None => romedb::CancellationToken::new(),
        };
        self.report(|connection| {
            connection
                .execute_atomic_request(&body, &token)
                .map(JsonText)
        })
    }
    #[napi]
    pub fn execute_cancellable(
        &self,
        sql: String,
        parameters: String,
        key: String,
    ) -> napi::Result<String> {
        let key = key.parse::<u64>().map_err(error)?;
        let token = cancellations()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
            .ok_or_else(|| error("unknown cancellation token"))?;
        self.report(|conn| {
            let parameters = decode_parameters(&parameters)?;
            query_value(conn.execute_cancellable(&sql, &parameters, &token)?)
        })
    }
    #[napi]
    pub fn profile_select(
        &self,
        sql: String,
        parameters: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let params = decode_parameters(&parameters)?;
            let profile = if let Some(token) = &token {
                conn.profile_select_cancellable(&sql, &params, token)?
            } else {
                conn.profile_select(&sql, &params)?
            };
            profile_value(profile)
        })
    }
    #[napi]
    pub fn profile_select_with_limits(
        &self,
        sql: String,
        parameters: String,
        max_rows: String,
        max_payload_bytes: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let params = decode_parameters(&parameters)?;
            let limits = romedb::ResultLimits {
                max_rows: max_rows
                    .parse()
                    .map_err(|_| romedb::Error::Validation("invalid result row limit".into()))?,
                max_payload_bytes: max_payload_bytes.parse().map_err(|_| {
                    romedb::Error::Validation("invalid result payload limit".into())
                })?,
            };
            let profile = if let Some(token) = &token {
                conn.profile_select_with_limits_cancellable(&sql, &params, limits, token)?
            } else {
                conn.profile_select_with_limits(&sql, &params, limits)?
            };
            profile_value(profile)
        })
    }
    #[napi]
    pub fn write_with_result_limits(
        &self,
        sql: String,
        parameters: String,
        max_rows: String,
        max_payload_bytes: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let params = decode_parameters(&parameters)?;
            let limits = romedb::ResultLimits {
                max_rows: max_rows
                    .parse()
                    .map_err(|_| romedb::Error::Validation("invalid result row limit".into()))?,
                max_payload_bytes: max_payload_bytes.parse().map_err(|_| {
                    romedb::Error::Validation("invalid result payload limit".into())
                })?,
            };
            let result = if let Some(token) = &token {
                conn.write_with_result_limits_cancellable(&sql, &params, limits, token)?
            } else {
                conn.write_with_result_limits(&sql, &params, limits)?
            };
            query_value(result)
        })
    }
    #[napi]
    pub fn check_collection_integrity(
        &self,
        table: String,
        max_documents: String,
        max_encoded_bytes: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let mut limits=romedb::IntegrityLimits::default();
            if !max_documents.is_empty() {
                limits.max_documents=max_documents.parse().map_err(|_|romedb::Error::Validation("invalid integrity document limit".into()))?;
            }
            if !max_encoded_bytes.is_empty() {
                limits.max_encoded_bytes=max_encoded_bytes.parse().map_err(|_|romedb::Error::Validation("invalid integrity byte limit".into()))?;
            }
            let report=if let Some(token) = &token { conn.check_collection_integrity_cancellable(&table,limits,token)? } else { conn.check_collection_integrity(&table,limits)? };
            Ok(serde_json::json!({"documents":report.documents.to_string(),"indexes":report.indexes.to_string(),"indexEntries":report.index_entries.to_string(),"encodedBytes":report.encoded_bytes.to_string()}))
        })
    }
    #[napi]
    pub fn execute_batch(
        &self,
        script: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let mut entries=String::from("[");
            let mut first=true;
            let visitor = |entry: romedb::BatchExecution| {
                let execution=entry.execution;
                let result=execution.result.and_then(query_value);
                let proceed=result.is_ok();
                if !first { entries.push(','); }
                first=false;
                entries.push('{');
                entries.push_str(&execution_field(result.map(|value| value.0)));
                let transaction=serde_json::json!({"before":execution.transaction_before,"after":execution.transaction_after});
                entries.push_str(&format!(",\"offset\":{},\"transaction\":{transaction}}}",entry.offset));
                Ok(proceed)
            };
            if let Some(token) = &token { conn.visit_batch_cancellable(&script, token, visitor)?; } else { conn.visit_batch(&script, visitor)?; }
            entries.push(']');
            Ok(JsonText(entries))
        })
    }
    #[napi]
    pub fn export_documents(
        &self,
        table: String,
        format: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let format = transfer_format(&format)?;
            let output = if let Some(token) = &token {
                conn.export_documents_cancellable(&table, format, token)?
            } else {
                conn.export_documents(&table, format)?
            };
            Ok(serde_json::Value::String(output))
        })
    }
    #[napi]
    pub fn import_documents(
        &self,
        table: String,
        input: String,
        format: String,
        cancellation_key: Option<String>,
    ) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let format = transfer_format(&format)?;
            let imported = if let Some(token) = &token {
                conn.import_documents_cancellable(&table, &input, format, token)?
            } else {
                conn.import_documents(&table, &input, format)?
            };
            Ok(serde_json::json!({"imported": imported}))
        })
    }
    #[napi]
    pub fn migrate(&self, input: String, cancellation_key: Option<String>) -> napi::Result<String> {
        let token = cancellation_key
            .map(|key| {
                let key = key.parse::<u64>().map_err(error)?;
                cancellations()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&key)
                    .cloned()
                    .ok_or_else(|| error("unknown cancellation token"))
            })
            .transpose()?;
        self.report(|conn| {
            let plan: serde_json::Value=romedb::decode_wire_json(&input)?;
            let invalid=||romedb::Error::Validation("expected migration objects with version, name and sql strings".into());
            let plan=plan.as_array().ok_or_else(invalid)?.iter().map(|m| {
                let field=|name|m.get(name).and_then(|v|v.as_str()).ok_or_else(invalid);
                let version=field("version")?.parse::<i64>().map_err(|_|invalid())?;
                Ok(romedb::Migration {version,name:field("name")?.into(),sql:field("sql")?.into()})
            }).collect::<romedb::Result<Vec<_>>>()?;
            let report=if let Some(token) = &token { conn.migrate_cancellable(&plan, token)? } else { conn.migrate(&plan)? };
            Ok(serde_json::json!({"alreadyApplied":report.already_applied,"applied":report.applied.iter().map(i64::to_string).collect::<Vec<_>>()}))
        })
    }
}
fn transfer_format(format: &str) -> romedb::Result<romedb::TransferFormat> {
    match format {
        "json" => Ok(romedb::TransferFormat::Json),
        "ndjson" => Ok(romedb::TransferFormat::Ndjson),
        _ => Err(romedb::Error::Validation(
            "expected json or ndjson transfer format".into(),
        )),
    }
}
impl Drop for NativeDatabase {
    fn drop(&mut self) {
        self.close_inner();
    }
}
impl NativeDatabase {
    fn close_inner(&mut self) {
        interrupts()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.interrupt_key);
        self.inner.take();
    }
    fn report<T: ResponseJson>(
        &self,
        operation: impl FnOnce(&romedb::Connection) -> romedb::Result<T>,
    ) -> napi::Result<String> {
        let (conn, _) = self
            .inner
            .as_ref()
            .ok_or_else(|| error("database is closed"))?;
        let before = conn.transaction_state();
        let result = operation(conn).and_then(ResponseJson::into_json);
        let transaction = serde_json::json!({"before":before,"after":conn.transaction_state()});
        Ok(wrap_json(
            execution_field(result),
            "{\"version\":1,\"execution\":{",
            &format!("}},\"transaction\":{transaction}}}"),
        ))
    }
}

fn decode_parameters(input: &str) -> romedb::Result<romedb::Parameters> {
    let values: BTreeMap<String, serde_json::Value> = romedb::decode_wire_json(input)?;
    values
        .into_iter()
        .map(|(key, value)| romedb::Value::from_portable_value(value).map(|v| (key, v)))
        .collect()
}

// Only internally serialized JSON can enter this wrapper. SQL/user text must
// pass through serde or the validated portable serializer before composition.
struct JsonText(String);
trait ResponseJson {
    fn into_json(self) -> romedb::Result<String>;
}
impl ResponseJson for JsonText {
    fn into_json(self) -> romedb::Result<String> {
        Ok(self.0)
    }
}
impl ResponseJson for serde_json::Value {
    fn into_json(self) -> romedb::Result<String> {
        Ok(serde_json::to_string(&self)?)
    }
}
fn wrap_json(mut payload: String, prefix: &str, suffix: &str) -> String {
    payload.reserve(prefix.len() + suffix.len());
    payload.insert_str(0, prefix);
    payload.push_str(suffix);
    payload
}
fn execution_field(result: romedb::Result<String>) -> String {
    match result {
        Ok(value) => wrap_json(value, "\"result\":", ""),
        Err(error) => {
            let mut diagnostic =
                serde_json::json!({"code":error.code(),"message":error.to_string()});
            if let romedb::Error::Migration {
                version,
                offset,
                source,
            } = &error
            {
                diagnostic["migration"] = serde_json::json!({
                    "version":version.to_string(),"offset":offset.to_string(),
                    "cause":{"code":source.code(),"message":source.to_string()}
                });
            }
            format!("\"error\":{diagnostic}")
        }
    }
}
fn query_value(result: romedb::QueryResult) -> romedb::Result<JsonText> {
    let mut json = format!(
        "{{\"columns\":{},\"affected\":{},\"rows\":[",
        serde_json::to_string(&result.columns)?,
        serde_json::to_string(&result.affected.to_string())?
    )
    .into_bytes();
    for (row_index, row) in result.rows.into_iter().enumerate() {
        if row_index > 0 {
            json.push(b',');
        }
        json.push(b'[');
        for (column_index, value) in row.into_iter().enumerate() {
            if column_index > 0 {
                json.push(b',');
            }
            value.write_portable_json(&mut json)?;
        }
        json.push(b']');
    }
    json.extend_from_slice(b"]}");
    Ok(JsonText(String::from_utf8(json).map_err(|error| {
        romedb::Error::Storage(format!("invalid serialized result UTF-8: {error}"))
    })?))
}

fn profile_value(profile: romedb::ProfiledQuery) -> romedb::Result<JsonText> {
    let m = profile.metrics;
    let metrics = serde_json::json!({
        "rowsRead":m.rows_read.to_string(), "rowsWritten":m.rows_written.to_string(),
        "fullscanSteps":m.fullscan_steps.to_string(), "indexSteps":m.index_steps.to_string(),
        "vmSteps":m.vm_steps.to_string(), "sortOperations":m.sort_operations.to_string(),
        "btreeSeeks":m.btree_seeks.to_string(),
        "fetchBatches":m.fetch_batches.to_string(), "fetchRowsRead":m.fetch_rows_read.to_string(),
        "fetchVmSteps":m.fetch_vm_steps.to_string(),
        "graphAdjacencyEntries":m.graph_adjacency_entries.to_string(),
        "graphExpansions":m.graph_expansions.to_string(),
        "graphDocuments":m.graph_documents.to_string(),
        "graphRows":m.graph_rows.to_string(),
        "graphVisited":m.graph_visited.to_string(),
        "graphPeakBytes":m.graph_peak_bytes.to_string(),
        "spatialRanges":m.spatial_ranges.to_string(),
        "spatialRounds":m.spatial_rounds.to_string()
    });
    let result = query_value(profile.result)?;
    Ok(JsonText(wrap_json(
        result.0,
        &format!("{{\"metrics\":{metrics},\"result\":"),
        "}",
    )))
}
