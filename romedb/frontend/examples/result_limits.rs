//! Run with: cargo run --locked -p romedb --example result_limits
use romedb::{CancellationToken, Database, Parameters, ResultLimits, TransactionState, Value};

fn main() -> romedb::Result<()> {
    let database = Database::open(":memory:")?;
    let connection = database.connect()?;
    let parameters = Parameters::new();
    connection.execute("CREATE TABLE items", &parameters)?;
    connection.execute("INSERT INTO items {id:items:a,n:1}", &parameters)?;

    // The result has one column name byte and eight integer payload bytes.
    let one = ResultLimits {
        max_rows: 1,
        max_payload_bytes: 9,
    };
    let profile = connection.profile_select_with_limits("SELECT n FROM items", &parameters, one)?;
    assert_eq!(profile.result.rows, vec![vec![Value::Integer(1)]]);

    connection.execute("BEGIN", &parameters)?;
    connection.execute("INSERT INTO items {id:items:b,n:2}", &parameters)?;
    let write = "UPDATE items SET n=n+10 RETURNING n";
    let error = connection
        .write_with_result_limits(write, &parameters, one)
        .unwrap_err();
    assert_eq!(error.code(), "FDB_LIMIT");
    assert_eq!(connection.transaction_state(), TransactionState::Active);
    // Rejected output rolls back the UPDATE, preserving the prior pending insert.
    let two = ResultLimits {
        max_rows: 2,
        max_payload_bytes: 17,
    };
    assert_eq!(
        connection
            .select_with_limits("SELECT n FROM items ORDER BY n", &parameters, two)?
            .rows,
        vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
    let accepted = connection.write_with_result_limits(write, &parameters, two)?;
    assert_eq!(accepted.affected, 2);
    connection.execute("ROLLBACK", &parameters)?;

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = connection
        .write_with_result_limits_cancellable("DELETE FROM items", &parameters, one, &cancelled)
        .unwrap_err();
    assert_eq!(error.code(), "FDB_CANCELLED");
    assert_eq!(
        connection
            .select_with_limits("SELECT n FROM items", &parameters, one)?
            .rows,
        vec![vec![Value::Integer(1)]]
    );
    println!("Result limits, atomic rejection, retry and cancellation verified.");
    Ok(())
}
