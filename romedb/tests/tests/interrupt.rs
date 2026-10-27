use romedb::{Database, Parameters};
use std::time::{Duration, Instant};
#[test]
fn cross_thread_interrupt_rolls_back_write_and_allows_reuse() {
    let db = Database::open(":memory:").unwrap();
    let c = db.connect().unwrap();
    c.execute("CREATE TABLE numbers(x INTEGER)", &Parameters::new())
        .unwrap();
    let rows = (1..=100)
        .map(|i| format!("({i})"))
        .collect::<Vec<_>>()
        .join(",");
    c.execute(
        &format!("INSERT INTO numbers VALUES {rows}"),
        &Parameters::new(),
    )
    .unwrap();
    c.execute("CREATE TABLE sink(x INTEGER)", &Parameters::new())
        .unwrap();
    let interrupt = c.interrupt_handle();
    assert!(interrupt.interrupt()); // Idle requests must not cancel the next operation.
    c.execute("SELECT 1", &Parameters::new()).unwrap();
    let work = std::thread::spawn(move || {
        let result = c.execute_report(
            "INSERT INTO sink SELECT a.x FROM numbers a CROSS JOIN numbers b CROSS JOIN numbers c",
            &Parameters::new(),
        );
        (c, result)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !work.is_finished() && Instant::now() < deadline {
        interrupt.interrupt();
        std::thread::sleep(Duration::from_millis(1));
    }
    let (c, report) = work.join().unwrap();
    assert_eq!(report.result.unwrap_err().code(), "FDB_CANCELLED");
    assert_eq!(
        c.execute("SELECT count(*) FROM sink", &Parameters::new())
            .unwrap()
            .rows,
        vec![vec![romedb::Value::Integer(0)]]
    );
    c.execute("INSERT INTO sink VALUES (1)", &Parameters::new())
        .unwrap();
    drop(c);
    assert!(!interrupt.interrupt());
}
