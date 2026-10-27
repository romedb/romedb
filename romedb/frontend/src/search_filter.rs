use crate::{Connection, EngineValue, Error, Index, Record, Result, Value};
use std::collections::BTreeSet;

pub(crate) struct Filter {
    ids: BTreeSet<Vec<u8>>,
}
impl Filter {
    pub(crate) fn from_value(value: &Value, table: &str) -> Result<Self> {
        let Value::Array(values) = value else {
            return Err(Error::Validation(
                "search filter requires an array of typed record IDs".into(),
            ));
        };
        if values.len() > 4096 {
            return Err(Error::Limit("search filter exceeds 4096 IDs".into()));
        }
        let records = values
            .iter()
            .map(|value| match value {
                Value::Record(record) => Ok(record),
                _ => Err(Error::Validation(
                    "search filter entries must be typed record IDs".into(),
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        Self::new(records.into_iter(), table)
    }
    pub(crate) fn from_records(records: &[Record], table: &str) -> Result<Self> {
        if records.len() > 4096 {
            return Err(Error::Limit("search filter exceeds 4096 IDs".into()));
        }
        Self::new(records.iter(), table)
    }
    fn new<'a>(records: impl Iterator<Item = &'a Record>, table: &str) -> Result<Self> {
        let mut bytes = 1024 * 1024usize;
        let mut ids = BTreeSet::new();
        for record in records {
            let key_bytes = match &record.key {
                crate::Key::String(key) => key.len(),
                crate::Key::Integer(_) => 8,
            };
            if record.table.len().saturating_add(key_bytes) > bytes {
                return Err(Error::Limit(
                    "search filter exceeds 1 MiB encoded IDs".into(),
                ));
            }
            let record = crate::normalized_id(record, table)?;
            let encoded = Value::Record(record).encode_with_limit(Some(bytes))?;
            bytes = bytes
                .checked_sub(encoded.len())
                .ok_or_else(|| Error::Limit("search filter exceeds 1 MiB encoded IDs".into()))?;
            ids.insert(encoded);
        }
        Ok(Self { ids })
    }
    pub(crate) fn predicate(&self) -> String {
        if self.ids.is_empty() {
            return "0".into();
        }
        let values = self
            .ids
            .iter()
            .map(|id| {
                let hex = id
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                format!("x'{hex}'")
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("id IN ({values})")
    }
    pub(crate) fn nodes(&self, connection: &Connection, index: &Index) -> Result<BTreeSet<u64>> {
        let mut nodes = BTreeSet::new();
        let sql = format!(
            "SELECT rowid FROM {} INDEXED BY {} WHERE id=?1",
            crate::quote(&index.storage),
            crate::quote(&index.name)
        );
        for id in &self.ids {
            let rows = connection.run_customer(&sql, &[EngineValue::Blob(id.clone())])?;
            for row in rows {
                match row.as_slice() {
                    [EngineValue::Numeric(turso_core::Numeric::Integer(node))] if *node >= 0 => {
                        nodes.insert(*node as u64);
                    }
                    _ => return Err(Error::Storage("invalid filtered vector node".into())),
                }
            }
        }
        Ok(nodes)
    }
}

pub(crate) enum Input<'a> {
    Value(&'a Value),
    Records(&'a [Record]),
}
impl Input<'_> {
    pub(crate) fn resolve(self, table: &str) -> Result<Filter> {
        match self {
            Self::Value(value) => Filter::from_value(value, table),
            Self::Records(records) => Filter::from_records(records, table),
        }
    }
}

pub(crate) fn argument(
    expression: &turso_parser::ast::Expr,
    params: &crate::Parameters,
    consumed: &mut BTreeSet<String>,
) -> Result<Value> {
    let mut bytes = crate::links::FetchBudget {
        used: 0,
        limit: 2 * 1024 * 1024,
    };
    argument_inner(expression, params, consumed, &mut bytes)
}

fn argument_inner(
    expression: &turso_parser::ast::Expr,
    params: &crate::Parameters,
    consumed: &mut BTreeSet<String>,
    bytes: &mut crate::links::FetchBudget,
) -> Result<Value> {
    use turso_parser::ast::{Expr, Literal, UnaryOperator};
    match expression {
        Expr::Variable(variable) => {
            let name = variable
                .name
                .as_ref()
                .map_or_else(|| format!("?{}", variable.index), |name| name.to_string());
            consumed.insert(name.clone());
            let value = params.get(&name).ok_or(Error::Parameter(name))?;
            if matches!(value,Value::Array(values) if values.len()>4096) {
                return Err(Error::Limit("search filter exceeds 4096 IDs".into()));
            }
            bytes.charge(value)?;
            value.validate()?;
            Ok(value.clone())
        }
        Expr::Unary(UnaryOperator::Negative, value)
            if matches!(value.as_ref(), Expr::Literal(Literal::Numeric(_))) =>
        {
            let Expr::Literal(Literal::Numeric(number)) = value.as_ref() else {
                unreachable!()
            };
            format!("-{number}")
                .parse::<i64>()
                .map(Value::Integer)
                .map_err(|_| {
                    Error::Validation("search filter record key must be an integer".into())
                })
        }
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            order_by,
            within_group,
            filter_over,
        } if name.as_str() == "__romedb_h_array_new"
            && distinctness.is_none()
            && order_by.is_empty()
            && within_group.is_empty()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none() =>
        {
            if args.len() > 4096 {
                return Err(Error::Limit("search filter exceeds 4096 IDs".into()));
            }
            args.iter()
                .map(|arg| argument_inner(arg, params, consumed, bytes))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array)
        }
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            order_by,
            within_group,
            filter_over,
        } if matches!(
            name.as_str(),
            "__romedb_record_value" | "__romedb_h_type_record"
        ) && distinctness.is_none()
            && order_by.is_empty()
            && within_group.is_empty()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none() =>
        {
            let [table, key] = args.as_slice() else {
                return Err(Error::Validation(
                    "search filter record constructor requires table and key".into(),
                ));
            };
            let Value::String(table) = argument_inner(table, params, consumed, bytes)? else {
                return Err(Error::Validation(
                    "search filter record table must be a string".into(),
                ));
            };
            let key = match argument_inner(key, params, consumed, bytes)? {
                Value::String(key) => crate::Key::String(key),
                Value::Integer(key) => crate::Key::Integer(key),
                _ => {
                    return Err(Error::Validation(
                        "search filter record key must be a string or integer".into(),
                    ))
                }
            };
            Ok(Value::Record(Record { table, key }))
        }
        Expr::Literal(Literal::Numeric(number)) => number
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|_| Error::Validation("search filter record key must be an integer".into())),
        Expr::Parenthesized(values) if values.len() == 1 => {
            argument_inner(&values[0], params, consumed, bytes)
        }
        _ => {
            let value = crate::spatial::search_argument(expression, params, consumed)?;
            bytes.charge(&value)?;
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidate_resolution_uses_id_index_and_broken_indexes_fail() {
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute(
            "INSERT INTO docs {id:docs:a,v:vector32('[1,0]'),body:'alpha'}",
            &Default::default(),
        )
        .unwrap();
        c.execute("CREATE SEARCH INDEX vectors ON docs(v) USING VECTOR WITH(dimensions=2,metric='l2',quantization='f16')",&Default::default()).unwrap();
        c.execute(
            "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
            &Default::default(),
        )
        .unwrap();
        let collection = c.catalog("docs").unwrap();
        let vector = collection
            .indexes
            .iter()
            .find(|i| i.name == "vectors")
            .unwrap();
        let record = Record {
            table: "docs".into(),
            key: crate::Key::String("a".into()),
        };
        let plan = c
            .run(
                &format!(
                    "EXPLAIN QUERY PLAN SELECT rowid FROM {} INDEXED BY {} WHERE id=?1",
                    crate::quote(&vector.storage),
                    crate::quote(&vector.name)
                ),
                &[EngineValue::Blob(
                    Value::Record(record.clone()).encode().unwrap(),
                )],
            )
            .unwrap();
        assert!(plan.iter().flatten().any(|v| matches!(v,EngineValue::Text(t) if t.as_str().contains("SEARCH") && t.as_str().contains("vectors") && t.as_str().contains("id=?"))),"{plan:?}");
        eprintln!("filtered ANN ID lookup: {plan:?}");
        let filter = Filter::from_records(&[record], "docs").unwrap();
        assert_eq!(filter.nodes(&c, vector).unwrap().len(), 1);
        c.run("SAVEPOINT broken", &[]).unwrap();
        c.run("DROP INDEX vectors", &[]).unwrap();
        assert!(filter.nodes(&c, vector).is_err());
        c.run("ROLLBACK TO broken", &[]).unwrap();
        c.run("RELEASE broken", &[]).unwrap();
        c.run(
            &format!(
                "UPDATE {} SET graph=x'010203'",
                crate::quote(&vector.ann_state())
            ),
            &[],
        )
        .unwrap();
        c.discard_ann_cache(vector).unwrap();
        assert_eq!(
            c.execute(
                "SELECT * FROM search::vector('vectors',vector32('[1,0]'),1,array::new(docs:a))",
                &Default::default()
            )
            .unwrap_err()
            .code(),
            "FDB_STORAGE"
        );
        c.run("DROP INDEX words", &[]).unwrap();
        assert!(c
            .execute(
                "SELECT * FROM search::text('words','alpha',1,array::new(docs:a))",
                &Default::default()
            )
            .is_err());
    }
}
