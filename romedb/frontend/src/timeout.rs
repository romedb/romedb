use crate::{Error, Result};
use romeql_parser::Kind;
use std::time::Duration;

pub(crate) fn split(sql: &str) -> Result<Option<(&str, Duration)>> {
    if !sql
        .as_bytes()
        .windows(7)
        .any(|part| part.eq_ignore_ascii_case(b"timeout"))
    {
        return Ok(None);
    }
    let tokens = romeql_parser::tokenize(sql)?;
    let mut end = tokens.len();
    if end > 0 && tokens[end - 1].text == ";" {
        end -= 1;
    }
    if end < 4 {
        return Ok(None);
    }
    let tail = &tokens[end - 3..end];
    if tail[0].kind != Kind::Word || !tail[0].text.eq_ignore_ascii_case("timeout") {
        return Ok(None);
    }
    let mut depth = 0usize;
    for token in &tokens[..end - 3] {
        if token.kind == Kind::Symbol {
            match token.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                ";" if depth == 0 => {
                    return Err(Error::Validation("TIMEOUT requires one statement".into()))
                }
                _ => (),
            }
        }
    }
    if depth != 0 {
        return Ok(None);
    }
    if tail[1].kind != Kind::Number || tail[2].kind != Kind::Word {
        return Ok(None);
    }
    let duration = tail[1]
        .text
        .parse::<u64>()
        .map_err(|_| Error::Validation("TIMEOUT requires a nonnegative integer duration".into()))?;
    let factor = match tail[2].text.to_ascii_lowercase().as_str() {
        "ms" => 1,
        "s" => 1000,
        _ => return Err(Error::Validation("TIMEOUT units must be ms or s".into())),
    };
    let millis = duration
        .checked_mul(factor)
        .filter(|ms| *ms <= 86_400_000)
        .ok_or_else(|| Error::Validation("TIMEOUT must not exceed one day".into()))?;
    let first = tokens
        .first()
        .ok_or_else(|| Error::Validation("TIMEOUT requires a statement".into()))?;
    if first.kind != Kind::Word
        || !matches!(
            first.text.to_ascii_lowercase().as_str(),
            "select"
                | "with"
                | "explain"
                | "insert"
                | "replace"
                | "update"
                | "upsert"
                | "delete"
                | "create"
                | "drop"
                | "alter"
                | "define"
                | "remove"
                | "reindex"
                | "info"
        )
    {
        return Err(Error::Validation(
            "TIMEOUT is unsupported for transaction control or this statement family".into(),
        ));
    }
    Ok(Some((
        sql[..tail[0].start].trim_end(),
        Duration::from_millis(millis),
    )))
}

impl crate::Connection {
    pub(crate) fn with_statement_timeout<T>(
        &self,
        sql: &str,
        operation: impl FnOnce(&str) -> Result<T>,
    ) -> Result<T> {
        let started = std::time::Instant::now();
        match split(sql)? {
            Some((body, duration)) => {
                let token = crate::CancellationToken::with_deadline(started + duration);
                self.with_cancellation(&token, || operation(body))
            }
            None => operation(sql),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_suffix_preserves_sql_names_literals_parameters_and_nested_queries() {
        for sql in [
            "SELECT timeout FROM docs",
            "SELECT timeout + 5",
            "SELECT 1 AS timeout",
            "SELECT 'TIMEOUT 1s'",
            "SELECT $timeout",
            "UPDATE docs SET timeout=1",
            "SELECT [TIMEOUT 1s] FROM docs",
            "SELECT * FROM (SELECT 1 AS timeout)",
        ] {
            assert!(split(sql).unwrap().is_none(), "{sql}");
        }
        for sql in [
            "SELECT 1 TIMEOUT 2s;",
            "SELECT 1 timeout 2000ms /*comment*/",
            "SELECT 1 TIMEOUT 2000 ms",
        ] {
            let (body, duration) = split(sql).unwrap().unwrap();
            assert_eq!(body, "SELECT 1");
            assert_eq!(duration, Duration::from_secs(2));
        }
        assert_eq!(
            split("SELECT 1 TIMEOUT 0ms").unwrap().unwrap().1,
            Duration::ZERO
        );
        for sql in [
            "COMMIT TIMEOUT 1s",
            "ROLLBACK TIMEOUT 1s",
            "PRAGMA journal_mode=WAL TIMEOUT 1s",
            "SELECT 1; SELECT 2 TIMEOUT 1s",
            "SELECT 1 TIMEOUT 86401s",
            "SELECT 1 TIMEOUT 1.5s",
            "SELECT 1 TIMEOUT 1h",
        ] {
            assert!(split(sql).is_err(), "{sql}");
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use crate::{CancellationToken, Database, Parameters, TransactionState, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use turso_ext::{scalar, ResultCode, Value as ExtValue};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    #[scalar(name = "timeout_returning_pause")]
    fn timeout_returning_pause(args: &[ExtValue]) -> ExtValue {
        CALLS.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(250));
        ExtValue::from_integer(args[0].to_integer().unwrap())
    }
    #[test]
    fn expiry_after_mutation_restores_native_and_collection_writes_and_indexes() {
        for collection in [false, true] {
            let c = Database::open(":memory:").unwrap().connect().unwrap();
            let p = Parameters::new();
            unsafe {
                let api = c.engine._build_turso_ext();
                let status = (api.register_scalar_function)(
                    api.ctx,
                    c"timeout_returning_pause".as_ptr(),
                    1,
                    false,
                    0,
                    timeout_returning_pause,
                    None,
                    None,
                );
                c.engine._free_extension_ctx(api);
                assert_eq!(status, ResultCode::OK);
            }
            c.execute(
                if collection {
                    "CREATE TABLE docs"
                } else {
                    "CREATE TABLE docs(n INTEGER)"
                },
                &p,
            )
            .unwrap();
            c.execute("CREATE UNIQUE INDEX docs_n ON docs(n)", &p)
                .unwrap();
            c.execute("INSERT INTO docs(n) VALUES(1)", &p).unwrap();
            for outer in [false, true] {
                if outer {
                    c.execute("BEGIN", &p).unwrap();
                    c.execute("INSERT INTO docs(n) VALUES(2)", &p).unwrap();
                }
                let before = c.execute("SELECT n FROM docs ORDER BY n", &p).unwrap().rows;
                let caller =
                    CancellationToken::with_deadline(Instant::now() + Duration::from_secs(60));
                for sql in [
                    "UPDATE docs SET n=n+10 RETURNING timeout_returning_pause(n),doc::diff() TIMEOUT 200ms",
                    "DELETE FROM docs RETURNING timeout_returning_pause(n),doc::before() TIMEOUT 200ms",
                    "INSERT INTO docs(n) VALUES(3),(4) RETURNING timeout_returning_pause(n),doc::after() TIMEOUT 200ms",
                ] {
                    let sql=if collection {sql.to_owned()} else {sql.replace(",doc::diff()","").replace(",doc::before()","").replace(",doc::after()","")};
                    CALLS.store(0,Ordering::SeqCst);
                    let started=Instant::now();
                    let error=c.execute_cancellable(&sql,&p,&caller).unwrap_err();
                    assert_eq!(error.code(),"FDB_CANCELLED","{sql}: {error}");
                    assert_eq!(CALLS.load(Ordering::SeqCst),1,"{sql}");
                    eprintln!("TIMEOUT 200ms during native callback: {:?}, collection={collection}, outer={outer}",started.elapsed());
                    assert_eq!(c.transaction_state(),if outer {TransactionState::Active} else {TransactionState::Autocommit});
                    assert_eq!(c.execute("SELECT n FROM docs ORDER BY n",&p).unwrap().rows,before);
                    if collection {c.check_collection_integrity("docs",Default::default()).unwrap();}
                }
                if outer {
                    c.execute("ROLLBACK", &p).unwrap();
                }
                assert_eq!(
                    c.execute("SELECT n FROM docs", &p).unwrap().rows,
                    vec![vec![Value::Integer(1)]]
                );
            }
        }
    }

    #[test]
    fn cancellation_arriving_with_another_error_waits_until_cleanup_finishes() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        let p = Parameters::new();
        c.execute("CREATE TABLE docs", &p).unwrap();
        c.execute("BEGIN", &p).unwrap();
        c.execute("INSERT INTO docs {n:1}", &p).unwrap();
        let token = CancellationToken::new();
        c.with_cancellation(&token, || {
            let failure: crate::Result<()> = c.atomic(|| {
                c.execute("INSERT INTO docs {n:2}", &p)?;
                token.cancel();
                Err(crate::Error::Validation("candidate failed".into()))
            });
            assert_eq!(failure.unwrap_err().code(), "FDB_VALIDATION");
            assert_eq!(
                c.execute("SELECT 1", &p).unwrap_err().code(),
                "FDB_CANCELLED"
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(c.transaction_state(), TransactionState::Active);
        assert_eq!(
            c.execute("SELECT n FROM docs", &p).unwrap().rows,
            vec![vec![Value::Integer(1)]]
        );
        c.execute("ROLLBACK", &p).unwrap();
    }

    #[test]
    fn nested_deadline_restores_parent_callback_and_delivers_only_once_during_cleanup() {
        let c = Database::open(":memory:").unwrap().connect().unwrap();
        let p = Parameters::new();
        let parent = CancellationToken::new();
        c.with_cancellation(&parent, || {
            c.execute("SELECT 1 TIMEOUT 1s", &p)?;
            parent.cancel();
            assert_eq!(
                c.execute("SELECT 2", &p).unwrap_err().code(),
                "FDB_CANCELLED"
            );
            assert_eq!(
                c.execute("SELECT 3", &p)?.rows,
                vec![vec![Value::Integer(3)]]
            );
            Ok(())
        })
        .unwrap();
        let fresh = CancellationToken::new();
        c.with_cancellation(&fresh, || {
            assert_eq!(
                c.execute("SELECT 1 TIMEOUT 0ms", &p).unwrap_err().code(),
                "FDB_CANCELLED"
            );
            fresh.cancel();
            assert_eq!(
                c.execute("SELECT 4", &p)?.rows,
                vec![vec![Value::Integer(4)]]
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            c.execute("SELECT 5", &p).unwrap().rows,
            vec![vec![Value::Integer(5)]]
        );
    }
}
