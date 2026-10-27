//! Shared lossless request handling for RomeDB clients.
use serde::Deserialize;
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

pub fn diagnostic(error: &romedb::Error) -> Json {
    let mut value = json!({"code":error.code(),"message":error.to_string()});
    if let romedb::Error::Migration {
        version,
        offset,
        source,
    } = error
    {
        value["migration"] = json!({"version":version,"offset":offset,"cause":diagnostic(source)});
    }
    value
}
pub fn invalid(message: &str) -> romedb::Error {
    romedb::Error::Validation(message.into())
}
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    max_rows: usize,
    max_payload_bytes: usize,
}
impl From<Limits> for romedb::ResultLimits {
    fn from(value: Limits) -> Self {
        Self {
            max_rows: value.max_rows,
            max_payload_bytes: value.max_payload_bytes,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    version: i64,
    name: String,
    sql: String,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Vector {
        encoding: String,
        components: Vec<f64>,
    },
    State,
    Execute {
        sql: String,
        parameters: BTreeMap<String, Json>,
        limits: Option<Limits>,
        #[serde(default)]
        write: bool,
    },
    Profile {
        sql: String,
        parameters: BTreeMap<String, Json>,
        limits: Option<Limits>,
    },
    Batch {
        sql: String,
    },
    Migrate {
        migrations: Vec<Migration>,
    },
    Export {
        table: String,
        format: String,
    },
    Import {
        table: String,
        format: String,
        input: String,
    },
    Integrity {
        table: String,
        max_documents: u64,
        max_encoded_bytes: u64,
    },
}
fn parameters(values: BTreeMap<String, Json>) -> romedb::Result<romedb::Parameters> {
    values
        .into_iter()
        .map(|(k, v)| romedb::Value::from_portable_value(v).map(|v| (k, v)))
        .collect()
}
fn query(result: romedb::QueryResult) -> romedb::Result<Json> {
    let rows = result
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(romedb::Value::into_portable_value)
                .collect::<romedb::Result<Vec<_>>>()
        })
        .collect::<romedb::Result<Vec<_>>>()?;
    Ok(json!({"columns":result.columns,"rows":rows,"affected":result.affected}))
}
fn format(name: &str) -> romedb::Result<romedb::TransferFormat> {
    match name {
        "json" => Ok(romedb::TransferFormat::Json),
        "ndjson" => Ok(romedb::TransferFormat::Ndjson),
        _ => Err(invalid("expected json or ndjson")),
    }
}
pub fn execute(
    conn: &romedb::Connection,
    request: Request,
    token: &romedb::CancellationToken,
) -> romedb::Result<Json> {
    match request {
        Request::Vector {
            encoding,
            components,
        } => vector(&encoding, &components)?.into_portable_value(),
        Request::State => Ok(serde_json::to_value(conn.transaction_state())?),
        Request::Execute {
            sql,
            parameters: p,
            limits,
            write,
        } => {
            let p = parameters(p)?;
            let result = match (limits, write) {
                (Some(limits), true) => {
                    conn.write_with_result_limits_cancellable(&sql, &p, limits.into(), token)?
                }
                (Some(limits), false) => {
                    conn.select_with_limits_cancellable(&sql, &p, limits.into(), token)?
                }
                (None, false) => conn.execute_cancellable(&sql, &p, token)?,
                (None, true) => return Err(invalid("write limits are required")),
            };
            query(result)
        }
        Request::Profile {
            sql,
            parameters: p,
            limits,
        } => {
            let p = parameters(p)?;
            let report = if let Some(limits) = limits {
                conn.profile_select_with_limits_cancellable(&sql, &p, limits.into(), token)?
            } else {
                conn.profile_select_cancellable(&sql, &p, token)?
            };
            Ok(json!({"result":query(report.result)?,"metrics":report.metrics}))
        }
        Request::Batch { sql } => {
            let reports = conn.execute_batch_cancellable(&sql, token)?;
            reports
                .into_iter()
                .map(|report| {
                    let execution = match report.execution.result {
                        Ok(result) => json!({"result": query(result)?}),
                        Err(error) => json!({"error": diagnostic(&error)}),
                    };
                    Ok(json!({
                        "offset": report.offset,
                        "transaction": {
                            "before": report.execution.transaction_before,
                            "after": report.execution.transaction_after,
                        },
                        "execution": execution,
                    }))
                })
                .collect::<romedb::Result<Vec<_>>>()
                .map(Json::Array)
        }
        Request::Migrate { migrations } => {
            let migrations = migrations
                .into_iter()
                .map(|m| romedb::Migration {
                    version: m.version,
                    name: m.name,
                    sql: m.sql,
                })
                .collect::<Vec<_>>();
            Ok(serde_json::to_value(
                conn.migrate_cancellable(&migrations, token)?,
            )?)
        }
        Request::Export { table, format: f } => Ok(Json::String(
            conn.export_documents_cancellable(&table, format(&f)?, token)?,
        )),
        Request::Import {
            table,
            format: f,
            input,
        } => Ok(json!(conn.import_documents_cancellable(
            &table,
            &input,
            format(&f)?,
            token
        )?)),
        Request::Integrity {
            table,
            max_documents,
            max_encoded_bytes,
        } => Ok(serde_json::to_value(
            conn.check_collection_integrity_cancellable(
                &table,
                romedb::IntegrityLimits {
                    max_documents,
                    max_encoded_bytes,
                },
                token,
            )?,
        )?),
    }
}

pub fn vector(encoding: &str, components: &[f64]) -> romedb::Result<romedb::Value> {
    if components.is_empty()
        || components.len() > 65_536
        || components.iter().any(|n| !n.is_finite())
    {
        return Err(invalid("vector needs 1..65536 finite components"));
    }
    if encoding == "float64" {
        return romedb::Value::vector64(components);
    }
    let values = components.iter().map(|n| *n as f32).collect::<Vec<_>>();
    match encoding {
        "float32" => romedb::Value::vector32(&values),
        "sparse32" => romedb::Value::vector32_sparse(&values),
        "quantized8" => romedb::Value::vector8(&values),
        "bit1" => romedb::Value::vector1bit(&values),
        _ => Err(invalid("unknown vector encoding")),
    }
}
