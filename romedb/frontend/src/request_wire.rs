use std::fmt;

type Result<T> = std::result::Result<T, RequestError>;
type Error = RequestError;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MAX_SEQUENCE: u64 = 9_007_199_254_740_990;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestEnvelope {
    pub request_id: String,
    pub after_sequence: u64,
    #[serde(default, deserialize_with = "present")]
    pub if_sequence: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    pub read_version: Option<u8>,
    pub statements: Vec<RequestStatement>,
    #[serde(default, deserialize_with = "present")]
    pub work_limits: Option<RequestWorkLimits>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestStatement {
    pub sql: String,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestWorkLimits {
    #[serde(default, deserialize_with = "present")]
    pub rows_read: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    pub row_mutations: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    pub vm_steps: Option<u64>,
}

impl RequestEnvelope {
    pub fn parse(body: &str, read_only: bool) -> Result<Self> {
        if body.len() > 65_536 {
            return Err(Error::new("FDB_LIMIT", "request exceeds 64 KiB"));
        }
        let request: Self = crate::decode_wire_json(body)?;
        if request.request_id.is_empty()
            || request.request_id.len() > 80
            || !request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            || request.after_sequence > MAX_SEQUENCE
            || request
                .if_sequence
                .is_some_and(|n| n < request.after_sequence || n > MAX_SEQUENCE)
            || request.statements.is_empty()
            || request.statements.len() > 32
        {
            return Err(Error::new(
                "FDB_VALIDATION",
                "invalid request identity, sequence or statement count",
            ));
        }
        if (read_only && (request.read_version != Some(2) || request.if_sequence.is_some()))
            || (!read_only && request.read_version.is_some())
        {
            return Err(Error::new(
                "FDB_VALIDATION",
                "reads require readVersion 2 without ifSequence; writes omit readVersion",
            ));
        }
        request.budget()?;
        Ok(request)
    }

    pub fn prepare(&self, read_only: bool) -> Result<Vec<PreparedRequestStatement>> {
        Ok(prepare_statements(&self.statements, read_only)?)
    }

    pub fn budget(&self) -> Result<crate::RequestBudget> {
        let limits = self.work_limits.as_ref();
        Ok(crate::RequestBudget::new(
            limits.and_then(|v| v.rows_read).unwrap_or(1_000_000),
            limits.and_then(|v| v.row_mutations).unwrap_or(100_000),
            limits.and_then(|v| v.vm_steps).unwrap_or(10_000_000),
        )?)
    }
}

pub struct PreparedRequestStatement<'a> {
    pub sql: &'a str,
    pub params: crate::Parameters,
    pub kind: crate::RequestStatementKind,
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    de: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(de).map(Some)
}

#[derive(Debug)]
pub struct RequestError {
    pub code: String,
    pub message: String,
}

impl RequestError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.chars().take(2048).collect(),
        }
    }
}
impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for RequestError {}
impl From<crate::Error> for RequestError {
    fn from(error: crate::Error) -> Self {
        Self::new(error.code(), &error.to_string())
    }
}

pub(crate) fn prepare_statements(
    statements: &[RequestStatement],
    read_only: bool,
) -> crate::Result<Vec<PreparedRequestStatement<'_>>> {
    if statements.is_empty() || statements.len() > 32 {
        return Err(crate::Error::Validation(
            "request requires 1..32 statements".into(),
        ));
    }
    statements
        .iter()
        .map(|statement| {
            let kind = crate::Connection::request_statement_kind(&statement.sql)?;
            if read_only
                && matches!(
                    kind,
                    crate::RequestStatementKind::Write | crate::RequestStatementKind::Schema
                )
            {
                return Err(crate::Error::Unsupported(
                    "fresh reads cannot execute writes or schema changes".into(),
                ));
            }
            let params = statement
                .params
                .iter()
                .map(|(key, value)| {
                    crate::Value::from_portable_value(value.clone())
                        .map_err(|error| match error {
                            crate::Error::Encoding(error) => crate::Error::Validation(format!(
                                "parameter {key} is not a portable value: {error}"
                            )),
                            error => error,
                        })
                        .map(|value| (key.clone(), value))
                })
                .collect::<crate::Result<_>>()?;
            Ok(PreparedRequestStatement {
                sql: &statement.sql,
                params,
                kind,
            })
        })
        .collect()
}
