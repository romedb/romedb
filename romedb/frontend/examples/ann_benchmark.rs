//! Bounded, reproducible frontend comparison; no general latency/recall guarantee.
use romedb::{Database, Document, Key, Parameters, Record, Value};
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::var("ANN_POINTS")
        .ok()
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10_000);
    let dims = 64;
    let queries = 16;
    let db = Database::open(":memory:")?;
    let c = db.connect()?;
    c.create_collection("items", false)?;
    let mut seed = 42u64;
    let mut vector = || -> romedb::Result<Value> {
        let values: Vec<f32> = (0..dims)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((seed >> 32) as u32 as f64 / u32::MAX as f64 * 2. - 1.) as f32
            })
            .collect();
        Value::vector32(&values)
    };
    c.execute("BEGIN", &Default::default())?;
    for key in 0..n {
        c.insert(
            "items",
            Document::from([
                (
                    "id".into(),
                    Value::Record(Record {
                        table: "items".into(),
                        key: Key::Integer(key as i64),
                    }),
                ),
                ("v".into(), vector()?),
            ]),
        )?;
    }
    c.execute("COMMIT", &Default::default())?;
    let start = Instant::now();
    c.create_vector_index(
        "items",
        "items_vec",
        vec!["v".into()],
        dims,
        "cosine",
        false,
    )?;
    let build = start.elapsed();
    let ann = "SELECT id,distance FROM search::vector('items_vec',$v,10) ORDER BY distance,id";
    let exact =
        "SELECT id,vector_distance_cos(v,$v) AS distance FROM items ORDER BY distance,id LIMIT 10";
    let mut recall = 0usize;
    let mut ann_us = 0;
    let mut exact_us = 0;
    let mut cold_us = 0;
    for query in 0..queries {
        let params = Parameters::from([("$v".into(), vector()?)]);
        let start = Instant::now();
        let result = c.execute(ann, &params)?;
        let elapsed = start.elapsed().as_micros();
        if query == 0 {
            cold_us = elapsed;
        } else {
            ann_us += elapsed;
        }
        let start = Instant::now();
        let expected = c.execute(exact, &params)?;
        exact_us += start.elapsed().as_micros();
        for row in &result.rows {
            if let Some(found) = expected.rows.iter().find(|other| row[0] == other[0]) {
                recall += 1;
                let (Value::Number(a), Value::Number(b)) = (&row[1], &found[1]) else {
                    panic!("distance type")
                };
                assert!((a - b).abs() < 1e-5, "distance mismatch {a} {b}");
            }
        }
    }
    let observed = recall as f64 / (queries * 10) as f64;
    println!("points={n} dimensions={dims} queries={queries} recall_at_10={observed:.4} build_ms={} cold_query_us={cold_us} warm_ann_avg_us={} exact_sql_avg_us={}",build.as_millis(),ann_us/(queries-1) as u128,exact_us/queries as u128);
    assert!(observed >= 0.95, "fixture recall below 0.95");
    c.check_collection_integrity("items", Default::default())?;
    Ok(())
}
