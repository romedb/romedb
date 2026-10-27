use reqwest::{blocking::Client, Method, Url};
use serde_json::{json, Value};
use std::io::{self, IsTerminal, Read, Write};
use std::time::Duration;
mod imports;
mod journal;
mod requests;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const LIMIT: usize = 64 * 1024;
const QUERY_LIMIT: usize = 1024 * 1024;
const QUERY_VERSION: u8 = 3;
const HELP: &str = "romedb cloud whoami\nromedb cloud organizations\nromedb cloud --organization ORG db create NAME\nromedb cloud --organization ORG db list\nromedb cloud --organization ORG db show UUID\nromedb cloud --organization ORG db delete UUID\nromedb cloud --organization ORG db access UUID\nromedb cloud --organization ORG db warm UUID\nromedb cloud --organization ORG db tokens UUID\nromedb cloud --organization ORG db token-create UUID ISSUANCE_UUID NAME read|write EXPIRES_AT_MS\nromedb cloud --organization ORG db token-revoke UUID TOKEN_UUID\nromedb cloud --organization ORG db read UUID JOURNAL < query.sql\nromedb cloud --organization ORG db query UUID JOURNAL < query.sql\nromedb cloud --organization ORG db retry JOURNAL\n\nSet ROMEDB_API_KEY to an organization key or database JWT (read/query/show/access/warm only). ROMEDB_ORGANIZATION_ID can supply ORG.\nROMEDB_CLOUD_URL defaults to https://cloud.romedb.com. JSON output goes to stdout.\nAccess accepts SQL/RomeQL ending in semicolons; batches commit atomically.\n.quit exits, .clear discards input, .retry retries an uncertain request in this session.\nRead and warm need read scope; query/access need read and query. Management needs manage.\nOne-shot journals retain SQL and request identity. Read retries run a fresh snapshot; queries replay their durable result.";

struct Cloud {
    http: Client,
    origin: Url,
    key: String,
    organization: Option<String>,
}
struct Failure {
    message: String,
    uncertain: bool,
    status: Option<u16>,
}
impl Cloud {
    fn new(endpoint: &str, key: String) -> Result<Self> {
        let endpoint = endpoint
            .strip_prefix("romedb://")
            .map(|rest| format!("https://{rest}"))
            .unwrap_or_else(|| endpoint.to_owned());
        let origin = Url::parse(&endpoint).map_err(|_| "Invalid ROMEDB_CLOUD_URL")?;
        let local = origin.host_str() == Some("localhost")
            || origin.host_str().is_some_and(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            });
        if !(origin.scheme() == "https" || origin.scheme() == "http" && local)
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(
                "ROMEDB_CLOUD_URL must be an HTTPS or romedb:// origin (HTTP is allowed only on loopback)"
                    .into(),
            );
        }
        let opaque = (key.starts_with("fdbk_") || key.starts_with("rdbo_"))
            && key.len() == 69
            && key[5..].bytes().all(|b| b.is_ascii_hexdigit());
        // This is only transport validation. The service verifies all JWT claims.
        let jwt = key.len() <= 4096
            && key.split('.').count() == 3
            && key.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            });
        if !opaque && !jwt {
            return Err("Set ROMEDB_API_KEY to a valid RomeDB API key or database token".into());
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| "Cannot initialize HTTPS client")?;
        Ok(Self {
            http,
            origin,
            key,
            organization: None,
        })
    }
    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Value, Failure> {
        self.send(method, path, body, 2)
    }
    fn query_request(
        &self,
        path: &str,
        body: &Value,
        query_version: u8,
    ) -> std::result::Result<Value, Failure> {
        self.send(Method::POST, path, Some(body), query_version)
    }
    fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query_version: u8,
    ) -> std::result::Result<Value, Failure> {
        let failure = |message: &str, uncertain| Failure {
            message: message.into(),
            uncertain,
            status: None,
        };
        let url = self
            .origin
            .join(path)
            .map_err(|_| failure("Invalid API path", false))?;
        let mut request = self.http.request(method, url).bearer_auth(&self.key);
        if query_version == QUERY_VERSION {
            request = request.header("x-romedb-query-version", QUERY_VERSION.to_string());
        }
        if let Some(body) = body {
            let encoded =
                serde_json::to_vec(body).map_err(|_| failure("Invalid query body", false))?;
            let limit = if query_version == QUERY_VERSION {
                QUERY_LIMIT
            } else {
                LIMIT
            };
            if encoded.len() > limit {
                return Err(failure(
                    if limit == QUERY_LIMIT {
                        "Request exceeds 1 MiB"
                    } else {
                        "Request exceeds 64 KiB"
                    },
                    false,
                ));
            }
            request = request
                .header("content-type", "application/json")
                .body(encoded);
        }
        self.response(request)
    }
    fn response(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> std::result::Result<Value, Failure> {
        let failure = |message: &str, uncertain| Failure {
            message: message.into(),
            uncertain,
            status: None,
        };
        let response = request
            .send()
            .map_err(|_| failure("Cloud request could not be acknowledged", true))?;
        let status = response.status();
        if status.as_u16() == 204 {
            return Ok(Value::Null);
        }
        let mut bytes = Vec::new();
        response
            .take(512 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| failure("Cloud response was interrupted", true))?;
        if bytes.len() > 512 * 1024 {
            return Err(failure("Cloud response exceeds 512 KiB", true));
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| failure("Invalid cloud response", true))?;
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Cloud request failed");
            return Err(Failure {
                message: format!(
                    "HTTP {}: {}",
                    status.as_u16(),
                    message.replace(&self.key, "[redacted]")
                ),
                uncertain: status.is_server_error() || status.is_redirection(),
                status: Some(status.as_u16()),
            });
        }
        Ok(value)
    }
    fn output(&self, value: &Value) -> Result<()> {
        println!(
            "{}",
            serde_json::to_string(value)?.replace(&self.key, "[redacted]")
        );
        Ok(())
    }
    fn organization(&self) -> Result<&str> {
        self.organization
            .as_deref()
            .ok_or_else(|| "Select --organization UUID or set ROMEDB_ORGANIZATION_ID".into())
    }
    fn collection_path(&self) -> Result<String> {
        Ok(format!(
            "/v1/organizations/{}/databases",
            self.organization()?
        ))
    }
    fn database_path(&self, id: &str) -> Result<String> {
        Ok(format!(
            "{}/{}",
            self.collection_path()?,
            canonical_uuid(id)?
        ))
    }
    fn warm_in_background(&self, path: &str) {
        let Ok(url) = self.origin.join(&format!("{path}/warm")) else {
            return;
        };
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.key)
            .header("content-type", "application/json")
            .timeout(Duration::from_secs(10))
            .body("{}");
        std::thread::spawn(move || {
            let _ = request.send();
        });
    }
    fn access(&self, id: &str) -> Result<std::process::ExitCode> {
        let path = self.database_path(id)?;
        let interactive = io::stdin().is_terminal();
        if interactive {
            self.warm_in_background(&path);
        }
        let mut stdin = io::stdin().lock();
        let mut reader: Box<dyn crate::input::Input + '_> =
            if interactive && crate::input::terminal_available() {
                Box::new(crate::input::Terminal::new(
                    None,
                    std::path::Path::new(":memory:"),
                )?)
            } else {
                Box::new(crate::input::Plain(&mut stdin))
            };
        let mut prompt: Box<dyn Write> = if interactive {
            Box::new(io::stderr())
        } else {
            Box::new(io::sink())
        };
        let mut buffer = String::new();
        let mut pending: Option<Value> = None;
        let mut pending_uncertain = false;
        let mut failed = false;
        let mut confirmed_sequence: Option<(u64, std::time::Instant)> = None;
        loop {
            let bytes = match reader.read(
                if buffer.is_empty() {
                    "romedb(cloud)> "
                } else {
                    "...> "
                },
                &mut prompt,
                QUERY_LIMIT,
            )? {
                crate::input::Read::Line(bytes) => bytes,
                crate::input::Read::Interrupted => {
                    buffer.clear();
                    continue;
                }
            };
            let eof = bytes.is_empty();
            let line = String::from_utf8(bytes)?;
            if line.trim() == ".quit" || line.trim() == ".exit" || eof && buffer.trim().is_empty() {
                if let Some(request) = pending {
                    eprintln!(
                        "Unresolved request {} at sequence {}; no new write was submitted",
                        request["requestId"], request["afterSequence"]
                    );
                    failed = true;
                }
                return Ok(if failed {
                    std::process::ExitCode::FAILURE
                } else {
                    std::process::ExitCode::SUCCESS
                });
            }
            if line.trim() == ".help" {
                writeln!(prompt, "{HELP}")?;
                continue;
            }
            if line.trim() == ".clear" {
                buffer.clear();
                continue;
            }
            if line.trim() != ".retry" {
                if pending.is_some() {
                    writeln!(
                        prompt,
                        "Use .retry to resolve the previous request before submitting more SQL"
                    )?;
                    failed = true;
                    continue;
                }
                if buffer.len() + line.len() > QUERY_LIMIT {
                    return Err("Cloud input exceeds 1 MiB".into());
                }
                buffer.push_str(&line);
                if !eof && !romeql_parser::script_complete(&buffer).unwrap_or(true) {
                    continue;
                }
                let statements = romeql_parser::split_script(&buffer)
                    .map_err(|_| "Invalid SQL/RomeQL script")?;
                if statements.is_empty() {
                    buffer.clear();
                    continue;
                }
                if statements.len() > 32 {
                    return Err("Cloud batches allow at most 32 statements".into());
                }
                let sequence = match confirmed_sequence.take() {
                    Some((value, at)) if at.elapsed() < Duration::from_secs(2) => value,
                    _ => requests::sequence(self, id)?,
                };
                pending_uncertain = false;
                pending = Some(
                    json!({ "requestId": uuid::Uuid::new_v4().to_string(), "afterSequence": sequence,
                    "statements": statements.iter().map(|statement| json!({"sql": statement.sql})).collect::<Vec<_>>() }),
                );
                buffer.clear();
            }
            let Some(request) = pending.as_ref() else {
                writeln!(prompt, "No request to retry")?;
                continue;
            };
            let response = self
                .query_request(&(path.clone() + "/query"), request, QUERY_VERSION)
                .and_then(|reply| {
                    if requests::confirmed(request, &reply) {
                        Ok(reply)
                    } else {
                        Err(Failure {
                            message: "Unconfirmed response identity; use .retry".into(),
                            uncertain: true,
                            status: None,
                        })
                    }
                });
            match response {
                Ok(result) => {
                    self.output(&result)?;
                    confirmed_sequence = requests::sole_commit_sequence(request, &result)
                        .map(|value| (value, std::time::Instant::now()));
                    pending = None;
                }
                Err(error) => {
                    confirmed_sequence = None;
                    self.output(&json!({ "error": error.message, "uncertain": pending_uncertain || error.uncertain, "requestId": request["requestId"], "afterSequence": request["afterSequence"] }))?;
                    pending_uncertain |= error.uncertain;
                    if !pending_uncertain {
                        pending = None;
                    }
                    failed = true;
                }
            }
        }
    }
}
fn canonical_uuid(value: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(value).map_err(|_| "Invalid UUID")?;
    if id.get_version_num() != 4 || id.to_string() != value {
        return Err("Expected a canonical lowercase version 4 UUID".into());
    }
    Ok(id.to_string())
}
pub fn run(mut args: Vec<String>) -> Result<std::process::ExitCode> {
    if args.is_empty() || args == ["--help"] || args == ["help"] {
        println!("{HELP}");
        println!("{}", imports::HELP);
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let mut cloud = Cloud::new(
        &std::env::var("ROMEDB_CLOUD_URL").unwrap_or_else(|_| "https://cloud.romedb.com".into()),
        std::env::var("ROMEDB_API_KEY")
            .map_err(|_| "Set ROMEDB_API_KEY before using cloud commands")?,
    )?;
    let selected = if args.first().map(String::as_str) == Some("--organization") {
        if args.len() < 3 {
            return Err(HELP.into());
        }
        let org = args.remove(1);
        args.remove(0);
        Some(org)
    } else {
        std::env::var("ROMEDB_ORGANIZATION_ID").ok()
    };
    cloud.organization = selected.as_deref().map(canonical_uuid).transpose()?;
    let parts: Vec<_> = args.iter().map(String::as_str).collect();
    if parts.first() == Some(&"import") {
        return imports::run(&cloud, &parts[1..]);
    }
    let (method, path, body) = match parts.as_slice() {
        ["whoami"] => (Method::GET, "/v1/whoami".into(), None),
        ["organizations"] => (Method::GET, "/v1/organizations".into(), None),
        ["db", "list"] => (Method::GET, cloud.collection_path()?, None),
        ["db", "create", name] => (
            Method::POST,
            cloud.collection_path()?,
            Some(json!({ "name": name })),
        ),
        ["db", "show", id] => (Method::GET, cloud.database_path(id)?, None),
        ["db", "delete", id] => (Method::DELETE, cloud.database_path(id)?, None),
        ["db", "tokens", id] => (
            Method::GET,
            format!("{}/tokens", cloud.database_path(id)?),
            None,
        ),
        ["db", "token-revoke", id, token] => (
            Method::DELETE,
            format!(
                "{}/tokens/{}",
                cloud.database_path(id)?,
                canonical_uuid(token)?
            ),
            None,
        ),
        ["db", "token-create", id, issuance, name, access, expiry] => {
            let scopes = match *access {
                "read" => vec!["read"],
                "write" => vec!["read", "query"],
                _ => return Err("Token access must be read or write".into()),
            };
            let expires_at: u64 = expiry
                .parse()
                .map_err(|_| "Expiry must be Unix milliseconds")?;
            (
                Method::POST,
                format!("{}/tokens", cloud.database_path(id)?),
                Some(json!({
                    "id": canonical_uuid(issuance)?, "name": name, "scopes": scopes, "expiresAt": expires_at
                })),
            )
        }
        ["db", "access", id] => return cloud.access(id),
        ["db", "warm", id] => (
            Method::POST,
            format!("{}/warm", cloud.database_path(id)?),
            Some(json!({})),
        ),
        ["db", operation @ ("read" | "query"), id, journal] => {
            return requests::start(&cloud, operation, id, journal)
        }
        ["db", "retry", journal] => return requests::retry(&cloud, journal),
        _ => return Err(HELP.into()),
    };
    let result = cloud
        .request(method, &path, body.as_ref())
        .map_err(|error| error.message)?;
    cloud.output(&result)?;
    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::Cloud;

    #[test]
    fn romedb_scheme_is_an_https_alias_with_the_same_origin_rules() {
        let key = format!("rdbo_{}", "a".repeat(64));
        let cloud = Cloud::new("romedb://cloud.romedb.com", key.clone()).unwrap();
        assert_eq!(cloud.origin.as_str(), "https://cloud.romedb.com/");
        for endpoint in [
            "romedb://user:pass@cloud.romedb.com",
            "romedb://cloud.romedb.com/?key=x",
            "romedb://cloud.romedb.com/path",
            "romedb:cloud.romedb.com",
        ] {
            assert!(Cloud::new(endpoint, key.clone()).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn organization_keys_must_use_the_rdbo_prefix() {
        let hex = "a".repeat(64);
        assert!(Cloud::new("https://cloud.romedb.com", format!("rdbo_{hex}")).is_ok());
        assert!(Cloud::new("https://cloud.romedb.com", format!("fdbo_{hex}")).is_err());
    }
}
