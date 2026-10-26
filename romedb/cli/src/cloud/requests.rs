//! Tracked one-shot reads and queries with immutable crash-recovery identity.
use super::{
    canonical_uuid, journal, Cloud, Method, Result, Value, LIMIT, QUERY_LIMIT, QUERY_VERSION,
};
use serde_json::json;
use std::io::{self, Read};
use std::path::Path;
use std::process::ExitCode;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_990;

fn validate_body(body: &Value, limit: usize) -> Result<()> {
    canonical_uuid(body["requestId"].as_str().ok_or("Missing request ID")?)?;
    if body["afterSequence"]
        .as_u64()
        .is_none_or(|s| s > MAX_SEQUENCE)
        || body.as_object().map(|o| o.len()) != Some(if body["readVersion"] == 2 { 4 } else { 3 })
    {
        return Err("Invalid request identity".into());
    }
    let statements = body["statements"].as_array().ok_or("Missing statements")?;
    if statements.is_empty()
        || statements.len() > 32
        || statements.iter().any(|s| {
            s.as_object().map(|o| o.len()) != Some(1)
                || s["sql"].as_str().is_none_or(|sql| sql.trim().is_empty())
        })
    {
        return Err("Expected 1 to 32 SQL statements".into());
    }
    if serde_json::to_vec(body)?.len() > limit {
        return Err(if limit == QUERY_LIMIT {
            "Request exceeds 1 MiB"
        } else {
            "Request exceeds 64 KiB"
        }
        .into());
    }
    Ok(())
}
pub(super) fn confirmed(body: &Value, reply: &Value) -> bool {
    reply["requestId"] == body["requestId"]
        && body["afterSequence"].as_u64().is_some_and(|after| {
            reply["sequence"].as_u64().is_some_and(|sequence| {
                sequence <= MAX_SEQUENCE + 1
                    && if body["readVersion"] == 2 {
                        reply["readVersion"] == 2 && sequence >= after
                    } else {
                        sequence > after && sequence - after <= 64
                    }
            })
        })
        && reply["results"].is_array()
}
pub(super) fn sole_commit_sequence(body: &Value, reply: &Value) -> Option<u64> {
    let after = body["afterSequence"].as_u64()?;
    let sequence = reply["sequence"].as_u64()?;
    (sequence == after + 1 && sequence <= MAX_SEQUENCE).then_some(sequence)
}
pub(super) fn sequence(cloud: &Cloud, id: &str) -> Result<u64> {
    let info = cloud
        .request(Method::GET, &cloud.database_path(id)?, None)
        .map_err(|e| e.message)?;
    if info["id"] != id {
        return Err("Database response identity differs".into());
    }
    info["sequence"]
        .as_u64()
        .filter(|n| *n <= MAX_SEQUENCE)
        .ok_or_else(|| "Invalid database sequence".into())
}
fn dispatch(cloud: &Cloud, value: &Value) -> Result<ExitCode> {
    let id = value["databaseId"]
        .as_str()
        .ok_or("Missing database UUID")?;
    let op = value["operation"].as_str().ok_or("Missing operation")?;
    let query_version = match (op, value["version"].as_u64()) {
        ("read", Some(3)) | ("query", Some(2)) => 2,
        ("read", Some(5)) | ("query", Some(4)) => QUERY_VERSION,
        _ => 0,
    };
    let limit = if query_version == QUERY_VERSION {
        QUERY_LIMIT
    } else {
        LIMIT
    };
    if query_version == 0
        || value["kind"] != "cloud-request"
        || value["origin"] != cloud.origin.as_str()
        || value["organizationId"] != cloud.organization()?
        || !["read", "query"].contains(&op)
        || value.as_object().map(|o| o.len()) != Some(7)
    {
        return Err(
            "Journal does not match the selected origin/organization or request format".into(),
        );
    }
    let path = format!("{}/{op}", cloud.database_path(id)?);
    let body = &value["request"];
    if (op == "read") != (body["readVersion"] == 2) {
        return Err(
            "Read journals require version 3 or 5 with readVersion 2; each retry runs a fresh snapshot"
                .into(),
        );
    }
    validate_body(body, limit)?;
    if op == "read" {
        eprintln!("Read retries execute again and may observe newer committed data.");
    }
    eprintln!(
        "Request {}. Keep the journal and use db retry after an uncertain reply.",
        body["requestId"]
    );
    match cloud.query_request(&path, body, query_version) {
        Ok(reply) => {
            if !confirmed(body, &reply) {
                return Err("Unconfirmed response identity; retry the same journal".into());
            }
            cloud.output(&reply)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            // The immutable journal can have earlier unknown dispatches. A new
            // rejection must never be presented as proof that they rolled back.
            cloud.output(&json!({"error":error.message,"outcome":"unresolved","requestId":body["requestId"],"afterSequence":body["afterSequence"]}))?;
            Ok(ExitCode::FAILURE)
        }
    }
}
pub(super) fn start(cloud: &Cloud, operation: &str, id: &str, path: &str) -> Result<ExitCode> {
    cloud.database_path(id)?;
    if Path::new(path).try_exists()? {
        return Err("Journal exists; use db retry rather than submitting a new request".into());
    }
    let mut sql = String::new();
    io::stdin()
        .take((QUERY_LIMIT + 1) as u64)
        .read_to_string(&mut sql)?;
    if sql.len() > QUERY_LIMIT {
        return Err("Cloud input exceeds 1 MiB".into());
    }
    let statements = romeql_parser::split_script(&sql).map_err(|_| "Invalid SQL/RomeQL script")?;
    let mut body = json!({"requestId":uuid::Uuid::new_v4().to_string(),"afterSequence":0,
        "statements":statements.iter().map(|s|json!({"sql":s.sql})).collect::<Vec<_>>()});
    if operation == "read" {
        body["readVersion"] = json!(2);
    }
    validate_body(&body, QUERY_LIMIT)?;
    body["afterSequence"] = json!(sequence(cloud, id)?);
    validate_body(&body, QUERY_LIMIT)?;
    let value = json!({"version":if operation == "read" { 5 } else { 4 },"kind":"cloud-request","origin":cloud.origin.as_str(),"organizationId":cloud.organization()?,
        "databaseId":id,"operation":operation,"request":body});
    journal::save(path, &value)?;
    dispatch(cloud, &value)
}
pub(super) fn retry(cloud: &Cloud, path: &str) -> Result<ExitCode> {
    dispatch(cloud, &journal::load(path, 2 * QUERY_LIMIT + 4096)?)
}
