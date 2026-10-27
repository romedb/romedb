use super::*;
use crate::{Collection, Index, IndexKind};

enum Target {
    Id,
    Index(Box<Index>),
    Any,
}

pub(super) struct Clause {
    target: Target,
    assignments: Option<Vec<(Vec<String>, Expr)>>,
    predicate: Option<Expr>,
}

impl Connection {
    pub(super) fn prepare_upsert(
        &self,
        table: &QualifiedName,
        first: &Upsert,
    ) -> Result<Vec<Clause>> {
        let collection = self.catalog(table.name.as_str())?;
        let mut clauses = Vec::new();
        let mut current = Some(first);
        while let Some(clause) = current {
            if clauses.len() == 16 {
                return Err(Error::Limit("ON CONFLICT exceeds 16 clauses".into()));
            }
            let target = if let Some(index) = &clause.index {
                if index.where_clause.is_some() {
                    return Err(unsupported("partial collection conflict targets"));
                }
                let paths = index
                    .targets
                    .iter()
                    .map(|target| {
                        if target.nulls.is_some() {
                            return Err(unsupported(
                                "NULL ordering in collection conflict targets",
                            ));
                        }
                        let path = static_path(&target.expr).ok_or_else(|| {
                            unsupported("expression or collated collection conflict targets")
                        })?;
                        crate::validate_path(&path)?;
                        Ok(path)
                    })
                    .collect::<Result<Vec<_>>>()?;
                if paths == [vec!["id".to_owned()]] {
                    Target::Id
                } else {
                    let index = collection
                        .indexes
                        .iter()
                        .find(|index| {
                            index.unique
                                && index.kind == IndexKind::Scalar
                                && index.paths().eq(paths.iter())
                        })
                        .ok_or_else(|| {
                            Error::Validation(
                                "ON CONFLICT target must match a declared unique index or id"
                                    .into(),
                            )
                        })?;
                    Target::Index(Box::new(index.clone()))
                }
            } else {
                if clause.next.is_some() {
                    return Err(Error::Validation(
                        "untargeted ON CONFLICT must be last".into(),
                    ));
                }
                Target::Any
            };
            let (assignments, predicate) = match &clause.do_clause {
                UpsertDo::Nothing => (None, None),
                UpsertDo::Set { sets, where_clause } => {
                    let mut assignments = Vec::new();
                    for set in sets {
                        let values = if set.col_names.len() == 1 {
                            vec![set.expr.as_ref()]
                        } else if let Expr::Parenthesized(values) = set.expr.as_ref() {
                            values.iter().map(Box::as_ref).collect()
                        } else {
                            return Err(unsupported("ON CONFLICT tuple subquery assignments"));
                        };
                        if values.len() != set.col_names.len() {
                            return Err(Error::Validation(
                                "ON CONFLICT assignment arity mismatch".into(),
                            ));
                        }
                        for (name, value) in set.col_names.iter().zip(values) {
                            safe_value_expression(value)?;
                            assignments.push((vec![name.as_str().to_owned()], value.clone()));
                        }
                    }
                    if assignments.len() > 1024 {
                        return Err(Error::Limit("ON CONFLICT exceeds 1024 assignments".into()));
                    }
                    crate::update::validate_targets(
                        &assignments
                            .iter()
                            .map(|(path, _)| path.clone())
                            .collect::<Vec<_>>(),
                    )?;
                    if let Some(predicate) = where_clause {
                        safe_value_expression(predicate)?;
                    }
                    (Some(assignments), where_clause.as_deref().cloned())
                }
            };
            clauses.push(Clause {
                target,
                assignments,
                predicate,
            });
            current = clause.next.as_deref();
        }
        Ok(clauses)
    }

    pub(super) fn insert_on_conflict(
        &self,
        table: &QualifiedName,
        mut document: Document,
        clauses: &[Clause],
        params: &Parameters,
        capture: bool,
    ) -> Result<Option<crate::mutation_result::Snapshot>> {
        let collection = self.collection_for_write(table.name.as_str())?;
        self.prepare_insert_document(&collection, &mut document)?;
        let mut budget = crate::budget::ResultBudget::new(
            Some(crate::ResultLimits {
                max_rows: usize::MAX,
                max_payload_bytes: 64 * 1024 * 1024,
            }),
            &[],
        )?;
        budget.document(&document)?;
        for clause in clauses {
            let Some(previous) = self.upsert_conflict(&collection, &document, &clause.target)?
            else {
                continue;
            };
            let Some(assignments) = &clause.assignments else {
                return Ok(None);
            };
            budget.document(&previous)?;
            for value in params.values() {
                budget.value(value)?;
            }
            let mut bound = params.clone();
            let mut expressions = assignments
                .iter()
                .map(|(_, expr)| expr.clone())
                .collect::<Vec<_>>();
            let mut predicate = clause.predicate.clone();
            let mut occupied = bound
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            for expr in expressions.iter().chain(predicate.iter()) {
                turso_core::walk_expr_mut(&mut expr.clone(), &mut |expr| {
                    if let Expr::Variable(var) = expr {
                        if let Some(name) = &var.name {
                            occupied.insert(name.to_string());
                        }
                    }
                    Ok(turso_core::WalkControl::Continue)
                })?;
            }
            for expr in expressions.iter_mut().chain(predicate.iter_mut()) {
                bind_excluded(expr, &document, &mut bound, &mut occupied, &mut budget)?;
            }
            let record = bind_value(&previous["id"], &mut bound, &mut occupied, &mut budget)?;
            let target = table.alias.as_ref().unwrap_or(&table.name);
            let identity =
                parse_expression(&format!("{}.id={record}", crate::quote(target.as_str())))?;
            let predicate = predicate.map_or(identity.clone(), |predicate| {
                Expr::Binary(Box::new(identity), Operator::And, Box::new(predicate))
            });
            let rows = self.write_candidates(
                table,
                WriteSource {
                    with: None,
                    from: None,
                },
                Some(Box::new(predicate)),
                &expressions,
                &bound,
            )?;
            let Some(row) = rows.into_iter().next() else {
                return Ok(None);
            };
            let mut updated = previous.clone();
            for ((path, _), value) in assignments.iter().zip(row.into_iter().skip(1)) {
                crate::update::apply(&mut updated, path, Some(value))?;
            }
            budget.document(&updated)?;
            self.replace_document(&collection, &mut updated)?;
            return Ok(Some(crate::mutation_result::Snapshot::after(
                updated,
                capture.then_some(previous),
            )));
        }
        self.store_insert_document(&collection, &document)?;
        Ok(Some(crate::mutation_result::Snapshot::after(
            document, None,
        )))
    }

    fn upsert_conflict(
        &self,
        collection: &Collection,
        document: &Document,
        target: &Target,
    ) -> Result<Option<Document>> {
        if matches!(target, Target::Id | Target::Any) {
            let conflict = self.get_in(collection, id(document)?)?;
            if conflict.is_some() || matches!(target, Target::Id) {
                return Ok(conflict);
            }
        }
        let indexes = match target {
            Target::Index(index) => vec![index.as_ref()],
            Target::Any => collection
                .indexes
                .iter()
                .filter(|index| index.unique)
                .collect(),
            Target::Id => unreachable!(),
        };
        for index in indexes {
            let keys = index.scalar_keys(document)?;
            if keys
                .iter()
                .any(|key| matches!(key, crate::EngineValue::Null))
            {
                continue;
            }
            let mut conflicts = self.lookup_scalar_keys(collection, index, &keys)?;
            if conflicts.len() > 1 {
                return Err(Error::Storage(
                    "unique index returned multiple conflicts".into(),
                ));
            }
            if let Some(conflict) = conflicts.pop() {
                return Ok(Some(conflict));
            }
        }
        Ok(None)
    }
}

fn static_path(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Id(name) | Expr::Name(name) => Some(vec![name.as_str().to_owned()]),
        Expr::Qualified(a, b) => Some(vec![a.as_str().into(), b.as_str().into()]),
        Expr::DoublyQualified(a, b, c) => Some(vec![
            a.as_str().into(),
            b.as_str().into(),
            c.as_str().into(),
        ]),
        Expr::Parenthesized(values) if values.len() == 1 => static_path(&values[0]),
        Expr::FunctionCall { name, args, .. } if name.as_str() == "__romedb_path" => args
            .iter()
            .map(|arg| match arg.as_ref() {
                Expr::Id(name) | Expr::Name(name) => Some(name.as_str().to_owned()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn parse_expression(sql: &str) -> Result<Expr> {
    let Cmd::Stmt(Stmt::Select(select)) = parsed(&format!("SELECT {sql}"))? else {
        unreachable!()
    };
    let OneSelect::Select { mut columns, .. } = select.body.select else {
        unreachable!()
    };
    let ResultColumn::Expr(expr, _) = columns.remove(0) else {
        unreachable!()
    };
    Ok(*expr)
}

fn bind_value(
    value: &Value,
    params: &mut Parameters,
    occupied: &mut std::collections::BTreeSet<String>,
    budget: &mut crate::budget::ResultBudget,
) -> Result<Expr> {
    budget.value(value)?;
    let mut index = occupied.len();
    let name = loop {
        let name = format!("$romedb_conflict_{index}");
        if occupied.insert(name.clone()) {
            break name;
        }
        index += 1;
    };
    let expr = parse_expression(&name)?;
    params.insert(name, value.clone());
    Ok(expr)
}

fn bind_excluded(
    expr: &mut Expr,
    candidate: &Document,
    params: &mut Parameters,
    occupied: &mut std::collections::BTreeSet<String>,
    budget: &mut crate::budget::ResultBudget,
) -> Result<()> {
    if let Some(path) =
        static_path(expr).filter(|path| path.len() > 1 && path[0].eq_ignore_ascii_case("excluded"))
    {
        let value = crate::path_value(candidate, &path[1..])?.unwrap_or(&Value::Null);
        *expr = bind_value(value, params, occupied, budget)?;
        return Ok(());
    }
    let mut failure = None;
    turso_core::walk_expr_mut(expr, &mut |expr| {
        if let Some(path) = static_path(expr)
            .filter(|path| path.len() > 1 && path[0].eq_ignore_ascii_case("excluded"))
        {
            let result = crate::path_value(candidate, &path[1..]).and_then(|value| {
                bind_value(value.unwrap_or(&Value::Null), params, occupied, budget)
            });
            match result {
                Ok(value) => *expr = value,
                Err(error) => failure = Some(error),
            }
            return Ok(turso_core::WalkControl::SkipChildren);
        }
        Ok(turso_core::WalkControl::Continue)
    })?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_after_conflict_writes_restores_all_indexes_and_prior_work() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        for sql in [
            "INSERT INTO docs {id:docs:a,a:1,b:2,tags:[1],body:'old'}",
            "CREATE UNIQUE INDEX pair ON docs(a,b)",
            "CREATE SEARCH INDEX tags ON docs(tags) USING ARRAY",
            "CREATE SEARCH INDEX words ON docs(body) USING FULLTEXT",
            "BEGIN",
            "INSERT INTO prior {n:7}",
        ] {
            c.execute(sql, &Parameters::new()).unwrap();
        }
        let baseline = c.engine.total_changes();
        let engine = Arc::downgrade(&c.engine);
        let fired = Arc::new(AtomicBool::new(false));
        let delivered = fired.clone();
        c.engine.set_progress_handler(
            1,
            Some(Box::new(move || {
                engine
                    .upgrade()
                    .is_some_and(|engine| engine.total_changes() > baseline)
                    && !delivered.swap(true, Ordering::SeqCst)
            })),
        );
        let result=c.execute("INSERT INTO docs(a,b,tags,body) VALUES(1,2,array::new(3),'new') ON CONFLICT(a,b) DO UPDATE SET tags=excluded.tags,body=excluded.body",&Parameters::new());
        c.engine.set_progress_handler(0, None);
        assert!(fired.load(Ordering::SeqCst));
        assert_eq!(result.unwrap_err().code(), "FDB_CANCELLED");
        c.check_collection_integrity("docs", Default::default())
            .unwrap();
        assert_eq!(
            c.execute(
                "SELECT body FROM docs WHERE array::contains(tags,1)",
                &Parameters::new()
            )
            .unwrap()
            .rows,
            vec![vec![Value::String("old".into())]]
        );
        c.execute("COMMIT", &Parameters::new()).unwrap();
        assert_eq!(
            c.execute("SELECT n FROM prior", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(7)]]
        );
    }

    #[test]
    fn excluded_copy_and_clause_limits_fail_without_mutation() {
        let c = crate::Database::open(":memory:")
            .unwrap()
            .connect()
            .unwrap();
        c.execute("INSERT INTO docs {id:docs:a,n:1}", &Parameters::new())
            .unwrap();
        let assignments = (0..80)
            .map(|i| format!("f{i}=excluded.body"))
            .collect::<Vec<_>>()
            .join(",");
        let sql=format!("INSERT INTO docs(id,body) VALUES(docs:a,$body) ON CONFLICT(id) DO UPDATE SET {assignments}");
        let result = c.execute(
            &sql,
            &Parameters::from([("$body".into(), Value::String("x".repeat(1024 * 1024)))]),
        );
        assert_eq!(result.unwrap_err().code(), "FDB_LIMIT");
        let clauses = " ON CONFLICT(id) DO NOTHING".repeat(17);
        assert_eq!(
            c.execute(
                &format!("INSERT INTO docs(id) VALUES(docs:b){clauses}"),
                &Parameters::new()
            )
            .unwrap_err()
            .code(),
            "FDB_LIMIT"
        );
        assert_eq!(
            c.execute("SELECT n FROM docs", &Parameters::new())
                .unwrap()
                .rows,
            vec![vec![Value::Integer(1)]]
        );
    }
}
