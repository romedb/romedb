//! Declared, indexed inverse reads. Expansion runs outside engine callbacks.
use crate::links::{FetchBudget, FetchMetrics, MAX_FETCH_BYTES, MAX_FETCH_REFERENCES};
use crate::{
    canonical, quote, Collection, Connection, Document, EngineValue, Error, Index, IndexKind,
    Parameters, Record, Result, Value,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use turso_parser::ast::{Expr, Literal, UnaryOperator};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Relation {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) path: Vec<String>,
    pub(crate) index: String,
}
impl Relation {
    pub(crate) fn validate(&self) -> Result<()> {
        for name in [&self.name, &self.source, &self.index] {
            if canonical(name)? != *name {
                return Err(Error::Storage("noncanonical relation metadata".into()));
            }
        }
        crate::validate_path(&self.path)
    }
}
pub(crate) fn validate_dependencies(collections: &[Collection]) -> Result<()> {
    let mut names = BTreeSet::new();
    for owner in collections {
        for relation in &owner.relations {
            if !names.insert(&relation.name) {
                return Err(Error::Storage("duplicate relation name".into()));
            }
            let source = collections
                .iter()
                .find(|c| c.name == relation.source)
                .ok_or_else(|| Error::Storage("missing relation source collection".into()))?;
            if !source.indexes.iter().any(|i| {
                i.name == relation.index && i.path == relation.path && i.kind == IndexKind::Scalar
            }) {
                return Err(Error::Storage(
                    "missing or incompatible relation reference index".into(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct PreparedRelation {
    relation: Relation,
    target: String,
    source: Collection,
    index: Index,
    limit: usize,
    after: Option<Vec<u8>>,
}
impl PreparedRelation {
    fn sql(&self) -> String {
        // Materialize bounded IDs before decoding documents. CROSS JOIN fixes
        // the selected IDs as the outer loop; documents use their primary key.
        format!("WITH selected AS MATERIALIZED (SELECT id FROM {} INDEXED BY {} WHERE \"key\"=?1 {} ORDER BY id LIMIT {}) SELECT d.doc FROM selected AS i CROSS JOIN {} AS d ON d.id=i.id ORDER BY i.id",
            quote(&self.index.storage), quote(&self.index.name),
            if self.after.is_some() { "AND id>?2" } else { "" }, self.limit, quote(&self.source.storage))
    }
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RequestKey {
    relation: String,
    target: Vec<u8>,
    limit: usize,
    after: Option<Vec<u8>>,
}

// Preparation-time values only: no row reads, volatile calls, or database re-entry.
fn argument(expr: &Expr, params: &Parameters, consumed: &mut BTreeSet<String>) -> Result<Value> {
    let invalid =
        || Error::Validation("relation arguments require literal or bound constants".into());
    match expr {
        Expr::Literal(Literal::Null) => Ok(Value::Null),
        Expr::Literal(Literal::Numeric(n)) => {
            if let Ok(n) = n.parse::<i64>() {
                return Ok(Value::Integer(n));
            }
            let n = n.parse::<f64>().map_err(|_| invalid())?;
            if !n.is_finite() {
                return Err(invalid());
            }
            Ok(Value::Number(n))
        }
        Expr::Literal(Literal::String(s)) => {
            let tokens = romeql_parser::tokenize(s)?;
            match tokens.as_slice() {
                [t] if t.kind == romeql_parser::Kind::String => Ok(Value::String(t.text.clone())),
                _ => Err(invalid()),
            }
        }
        Expr::Variable(v) => {
            let name = v
                .name
                .as_ref()
                .map_or_else(|| format!("?{}", v.index), |name| name.to_string());
            consumed.insert(name.clone());
            let value = params.get(&name).cloned().ok_or(Error::Parameter(name))?;
            value.validate()?;
            Ok(value)
        }
        Expr::Parenthesized(es) if es.len() == 1 => argument(&es[0], params, consumed),
        Expr::Unary(op @ (UnaryOperator::Positive | UnaryOperator::Negative), e) => {
            if matches!(op, UnaryOperator::Negative) {
                if let Expr::Literal(Literal::Numeric(n)) = e.as_ref() {
                    if let Ok(n) = format!("-{n}").parse::<i64>() {
                        return Ok(Value::Integer(n));
                    }
                }
            }
            let value = argument(e, params, consumed)?;
            match (op, value) {
                (UnaryOperator::Positive, value @ (Value::Integer(_) | Value::Number(_))) => {
                    Ok(value)
                }
                (_, Value::Integer(n)) => n.checked_neg().map(Value::Integer).ok_or_else(invalid),
                (_, Value::Number(n)) => Ok(Value::Number(-n)),
                _ => Err(invalid()),
            }
        }
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            order_by,
            within_group,
            filter_over,
        } if name.as_str() == "__romedb_record_value"
            && args.len() == 2
            && distinctness.is_none()
            && order_by.is_empty()
            && within_group.is_empty()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none() =>
        {
            let Value::String(table) = argument(&args[0], params, consumed)? else {
                return Err(invalid());
            };
            let key = match argument(&args[1], params, consumed)? {
                Value::Integer(n) => crate::Key::Integer(n),
                Value::String(s) => crate::Key::String(s),
                _ => return Err(invalid()),
            };
            let value = Value::Record(Record {
                table: canonical(&table)?,
                key,
            });
            value.validate()?;
            Ok(value)
        }
        _ => Err(invalid()),
    }
}

impl Connection {
    /// Declare an inverse lookup backed by an existing managed scalar index.
    pub fn define_relation(
        &self,
        name: &str,
        target: &str,
        source: &str,
        path: Vec<String>,
    ) -> Result<()> {
        let name = canonical(name)?;
        crate::validate_path(&path)?;
        self.atomic(|| {
            if self
                .collections()?
                .iter()
                .any(|c| c.relations.iter().any(|r| r.name == name))
            {
                return Err(Error::AlreadyExists(name.clone()));
            }
            let mut target = self.catalog(target)?;
            let source = self.catalog(source)?;
            let index = source
                .indexes
                .iter()
                .filter(|i| i.path == path && i.kind == IndexKind::Scalar)
                .min_by_key(|i| &i.name)
                .ok_or_else(|| {
                    Error::Validation("relation requires a scalar index on its source path".into())
                })?;
            target.relations.push(Relation {
                name: name.clone(),
                source: source.name.clone(),
                path: path.clone(),
                index: index.name.clone(),
            });
            self.save_catalog(&target)
        })
    }
    /// Remove a declaration; source documents and their index remain intact.
    pub fn drop_relation(&self, name: &str, if_exists: bool) -> Result<()> {
        let name = canonical(name)?;
        self.atomic(|| {
            for mut owner in self.collections()? {
                if let Some(position) = owner.relations.iter().position(|r| r.name == name) {
                    owner.relations.remove(position);
                    return self.save_catalog(&owner);
                }
            }
            if if_exists {
                Ok(())
            } else {
                Err(Error::NotFound(name.clone()))
            }
        })
    }
    fn relation(&self, name: &str) -> Result<(String, Relation)> {
        let name = canonical(name)?;
        for owner in self.collections()? {
            if let Some(relation) = owner.relations.into_iter().find(|r| r.name == name) {
                return Ok((owner.name, relation));
            }
        }
        Err(Error::NotFound(format!("relation {name}")))
    }
    pub(crate) fn relation_info(&self, name: &str) -> Result<Value> {
        let (target, relation) = self.relation(name)?;
        Ok(Value::Object(Document::from([
            ("name".into(), Value::String(relation.name)),
            ("target".into(), Value::String(target)),
            ("source".into(), Value::String(relation.source)),
            (
                "path".into(),
                Value::Array(relation.path.into_iter().map(Value::String).collect()),
            ),
            ("index".into(), Value::String(relation.index)),
        ])))
    }
    pub(crate) fn prepare_relation(
        &self,
        args: &[Box<Expr>],
        params: &Parameters,
        consumed: &mut BTreeSet<String>,
    ) -> Result<PreparedRelation> {
        let Value::String(name) = argument(&args[1], params, consumed)? else {
            return Err(Error::Validation("relation name must be a string".into()));
        };
        let (target, relation) = self.relation(&name)?;
        let source = self.catalog(&relation.source)?;
        let index = source
            .indexes
            .iter()
            .find(|i| {
                i.name == relation.index && i.path == relation.path && i.kind == IndexKind::Scalar
            })
            .cloned()
            .ok_or_else(|| Error::Storage("relation reference index is missing".into()))?;
        let limit = if args.len() >= 3 {
            match argument(&args[2], params, consumed)? {
                Value::Integer(n) if (0..=1000).contains(&n) => n as usize,
                Value::Number(n) if (0.0..=1000.0).contains(&n) && n.fract() == 0.0 => n as usize,
                _ => {
                    return Err(Error::Validation(
                        "relation limit must be an integer from 0 through 1000".into(),
                    ))
                }
            }
        } else {
            100
        };
        let after = if args.len() == 4 {
            match argument(&args[3], params, consumed)? {
                Value::Null => None,
                Value::Record(r) => {
                    Some(Value::Record(crate::normalized_id(&r, &source.name)?).encode()?)
                }
                _ => {
                    return Err(Error::Validation(
                        "relation cursor must be a source record or null".into(),
                    ))
                }
            }
        } else {
            None
        };
        Ok(PreparedRelation {
            relation,
            target,
            source,
            index,
            limit,
            after,
        })
    }
    pub(crate) fn fetch_relations(
        &self,
        requests: &[(usize, Value)],
        specs: &BTreeMap<usize, PreparedRelation>,
        result_budget: &mut crate::budget::ResultBudget,
    ) -> Result<(Vec<Value>, FetchMetrics)> {
        self.fetch_relations_bounded(requests, specs, result_budget, MAX_FETCH_BYTES)
    }
    fn fetch_relations_bounded(
        &self,
        requests: &[(usize, Value)],
        specs: &BTreeMap<usize, PreparedRelation>,
        result_budget: &mut crate::budget::ResultBudget,
        max_bytes: usize,
    ) -> Result<(Vec<Value>, FetchMetrics)> {
        if requests.len() > MAX_FETCH_REFERENCES {
            return Err(Error::Limit("fetch reference count exceeds 16384".into()));
        }
        let mut groups = BTreeMap::new();
        let mut identities = Vec::with_capacity(requests.len());
        for (position, value) in requests {
            let spec = &specs[position];
            let record = match value {
                Value::Null => {
                    identities.push(None);
                    continue;
                }
                Value::Record(r) => crate::normalized_id(r, &spec.target)?,
                _ => {
                    return Err(Error::Validation(
                        "relation::fetch expects a typed target record or null".into(),
                    ))
                }
            };
            let key = RequestKey {
                relation: spec.relation.name.clone(),
                target: Value::Record(record.clone()).encode()?,
                limit: spec.limit,
                after: spec.after.clone(),
            };
            groups.entry(key.clone()).or_insert((spec, record));
            identities.push(Some(key));
        }
        let mut found = BTreeMap::new();
        let mut target_budget = FetchBudget {
            used: 0,
            limit: max_bytes,
        };
        let mut metrics = FetchMetrics::default();
        for (key, (spec, record)) in groups {
            let mut values = Vec::new();
            if spec.limit != 0 {
                let mut statement = self.prepare(spec.sql())?;
                statement.bind_at(
                    std::num::NonZeroUsize::new(1).unwrap(),
                    crate::index_scalar(&Value::Record(record))?,
                )?;
                if let Some(after) = &spec.after {
                    statement.bind_at(
                        std::num::NonZeroUsize::new(2).unwrap(),
                        EngineValue::Blob(after.clone()),
                    )?;
                }
                crate::links::visit_target_rows(self, &mut statement, |row| {
                    let value = Value::Object(crate::decode_document(&row[0])?);
                    target_budget.charge(&value)?;
                    values.push(value);
                    Ok(())
                })?;
                metrics.add(&statement);
            }
            let value = Value::Array(values);
            value.validate()?;
            found.insert(key, value);
        }
        // Account for every duplicate occurrence before allocating cloned output.
        let mut output_budget = FetchBudget {
            used: 0,
            limit: max_bytes,
        };
        for id in &identities {
            let value = id.as_ref().map_or(&Value::Null, |id| &found[id]);
            output_budget.charge(value)?;
            result_budget.value(value)?;
        }
        Ok((
            identities
                .into_iter()
                .map(|id| id.map_or(Value::Null, |id| found[&id].clone()))
                .collect(),
            metrics,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    fn record(table: &str, key: &str) -> Record {
        Record {
            table: table.into(),
            key: crate::Key::String(key.into()),
        }
    }
    fn q(c: &Connection, sql: &str) -> crate::QueryResult {
        c.execute(sql, &Parameters::new()).unwrap()
    }
    fn setup(c: &Connection) {
        q(c, "CREATE TABLE users");
        q(
            c,
            "INSERT INTO posts {id:posts:a,author:users:u1,title:'A'}",
        );
        q(c, "CREATE INDEX authors ON posts(author)");
        q(c, "DEFINE RELATION authored ON users FROM posts.author");
    }
    fn spec(c: &Connection) -> PreparedRelation {
        let mut consumed = BTreeSet::new();
        c.prepare_relation(
            &[
                Box::new(Expr::Literal(Literal::Null)),
                Box::new(Expr::Literal(Literal::String("'authored'".into()))),
            ],
            &Parameters::new(),
            &mut consumed,
        )
        .unwrap()
    }
    #[test]
    fn inverse_metadata_dependencies_and_indexed_plan_are_verified() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        setup(&c);
        assert_eq!(c.catalog("users").unwrap().version, 3);
        assert_eq!(c.catalog("posts").unwrap().version, 2);
        let plan = c
            .run(
                &format!("EXPLAIN QUERY PLAN {}", spec(&c).sql()),
                &[crate::index_scalar(&Value::Record(record("users", "u1"))).unwrap()],
            )
            .unwrap();
        let plan = format!("{plan:?}");
        assert!(plan.contains("authors"), "{plan}");
        assert!(plan.contains("SEARCH d"), "{plan}");
        assert!(!plan.contains("SCAN d"), "{plan}");
        eprintln!("inverse query plan: {plan}");
        let original = serde_json::to_value(c.catalog("users").unwrap()).unwrap();
        for (pointer, value) in [
            ("/version", serde_json::json!(2)),
            ("/relations/0/index", serde_json::json!("missing")),
            ("/relations/0/source", serde_json::json!("missing")),
            ("/relations/0/path", serde_json::json!(["different"])),
            ("/relations/0/name", serde_json::json!("AUTHORED")),
        ] {
            let mut invalid = original.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            c.run(
                "UPDATE __romedb_catalog SET metadata=?1 WHERE name='users'",
                &[crate::text(&invalid.to_string())],
            )
            .unwrap();
            assert!(db.connect().is_err(), "{pointer}");
        }
        c.run(
            "UPDATE __romedb_catalog SET metadata=?1 WHERE name='users'",
            &[crate::text(&original.to_string())],
        )
        .unwrap();
        db.connect().unwrap();
        q(&c, "DROP RELATION authored");
        assert_eq!(c.catalog("users").unwrap().version, 3);
    }
    #[test]
    fn inverse_expansion_limits_count_duplicate_arrays_and_nulls() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        setup(&c);
        let spec = spec(&c);
        let specs = BTreeMap::from([(0, spec)]);
        let request = (0, Value::Record(record("users", "u1")));
        let requests = vec![request.clone(), request.clone()];
        let mut unlimited = crate::budget::ResultBudget::new(None, &[]).unwrap();
        let (expected, metrics) = c
            .atomic(|| c.fetch_relations(&requests, &specs, &mut unlimited))
            .unwrap();
        assert_eq!(metrics.batches, 1);
        let bytes = serde_json::to_vec(&expected[0]).unwrap().len() * 2;
        for (limit, ok) in [(bytes, true), (bytes - 1, false), (0, false)] {
            let mut budget = crate::budget::ResultBudget::new(None, &[]).unwrap();
            let result =
                c.atomic(|| c.fetch_relations_bounded(&requests, &specs, &mut budget, limit));
            if ok {
                assert_eq!(result.unwrap().0, expected);
            } else {
                assert_eq!(result.err().unwrap().code(), "FDB_LIMIT");
            }
        }
        let mut budget = crate::budget::ResultBudget::new(
            Some(crate::ResultLimits {
                max_rows: 1,
                max_payload_bytes: 1,
            }),
            &[],
        )
        .unwrap();
        assert_eq!(
            c.fetch_relations(&[(0, Value::Null)], &specs, &mut budget)
                .unwrap()
                .0,
            vec![Value::Null]
        );
        let mut budget = crate::budget::ResultBudget::new(None, &[]).unwrap();
        assert_eq!(
            c.fetch_relations(
                &vec![request; MAX_FETCH_REFERENCES + 1],
                &specs,
                &mut budget
            )
            .err()
            .unwrap()
            .code(),
            "FDB_LIMIT"
        );
        // The returned array is part of the logical value's depth budget.
        let mut deep = Value::Null;
        for _ in 0..63 {
            deep = Value::Array(vec![deep]);
        }
        c.patch(
            &record("posts", "a"),
            Document::from([("deep".into(), deep)]),
        )
        .unwrap();
        assert_eq!(
            c.profile_select(
                "SELECT relation::fetch(users:u1,'authored')",
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert!(c.get(&record("posts", "a")).unwrap().is_some());
    }
    #[test]
    fn inverse_interruption_preserves_pending_work_and_retry() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        setup(&c);
        q(&c, "BEGIN");
        for n in 0..40 {
            q(
                &c,
                &format!("INSERT INTO posts {{id:posts:p{n},author:users:u1}}"),
            );
        }
        let sql = "SELECT relation::fetch(users:u1,'authored',20)";
        let count = Arc::new(AtomicUsize::new(0));
        let ticks = count.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                ticks.fetch_add(1, Ordering::SeqCst);
                false
            })),
        );
        let baseline = c.profile_select(sql, &Parameters::new());
        c.engine.set_progress_handler(0, None);
        let baseline = baseline.unwrap();
        let total = count.load(Ordering::SeqCst);
        assert!(total > 100);
        for stop in [total / 2, total * 3 / 4, total - 1] {
            let count = Arc::new(AtomicUsize::new(0));
            let fired = Arc::new(AtomicBool::new(false));
            let tick = count.clone();
            let flag = fired.clone();
            c.engine.set_progress_handler(
                1,
                Some(Box::new(move || {
                    tick.fetch_add(1, Ordering::SeqCst) + 1 >= stop
                        && !flag.swap(true, Ordering::SeqCst)
                })),
            );
            let result = c.profile_select(sql, &Parameters::new());
            c.engine.set_progress_handler(0, None);
            assert!(fired.load(Ordering::SeqCst));
            assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
            assert_eq!(c.transaction_state(), crate::TransactionState::Active);
            let retry = c.profile_select(sql, &Parameters::new()).unwrap();
            assert_eq!(retry.result.rows, baseline.result.rows);
            assert_eq!(retry.metrics, baseline.metrics);
        }
        assert_eq!(
            c.check_collection_integrity("posts", Default::default())
                .unwrap()
                .documents,
            41
        );
        q(&c, "ROLLBACK");
        assert_eq!(
            c.check_collection_integrity("posts", Default::default())
                .unwrap()
                .documents,
            1
        );
    }
}
