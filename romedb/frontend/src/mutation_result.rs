use crate::{Document, Error, Result, Value};

pub(crate) struct Snapshot {
    pub document: Document,
    pub before: Option<Document>,
    pub deleted: bool,
}

impl Snapshot {
    pub(crate) fn charge(&self, budget: &mut crate::budget::ResultBudget) -> Result<()> {
        budget.document(&self.document)?;
        if let Some(before) = &self.before {
            budget.document_fields(before)?;
        }
        Ok(())
    }
    pub(crate) fn after(document: Document, before: Option<Document>) -> Self {
        Self {
            document,
            before,
            deleted: false,
        }
    }
    pub(crate) fn deleted(document: Document) -> Self {
        Self {
            document,
            before: None,
            deleted: true,
        }
    }
    pub(crate) fn before(&self) -> Option<&Document> {
        if self.deleted {
            Some(&self.document)
        } else {
            self.before.as_ref()
        }
    }
    pub(crate) fn after_document(&self) -> Option<&Document> {
        (!self.deleted).then_some(&self.document)
    }
}

pub(crate) fn diff(before: Option<&Document>, after: Option<&Document>) -> Result<Value> {
    struct Diff {
        changes: Vec<Value>,
        bytes: crate::links::FetchBudget,
        work: usize,
    }
    impl Diff {
        fn change(&mut self, op: &str, path: &str, value: Option<&Value>) -> Result<()> {
            if self.changes.len() >= 1024 {
                return Err(Error::Limit("mutation diff exceeds 1024 operations".into()));
            }
            if path.len() > 16_384 {
                return Err(Error::Limit(
                    "mutation diff pointer exceeds 16384 bytes".into(),
                ));
            }
            let mut entry = Document::from([
                ("op".into(), Value::String(op.into())),
                ("path".into(), Value::String(path.into())),
            ]);
            if let Some(value) = value {
                self.bytes.charge(value)?;
                entry.insert("value".into(), value.clone());
            }
            let value = Value::Object(entry);
            self.bytes.charge(&value)?;
            self.changes.push(value);
            Ok(())
        }
        fn equal(&mut self, before: &Value, after: &Value) -> Result<bool> {
            if self.work == 0 {
                return Err(Error::Limit("mutation diff exceeds 1000000 steps".into()));
            }
            self.work -= 1;
            match (before, after) {
                (Value::Array(before), Value::Array(after)) => {
                    if before.len() != after.len() {
                        return Ok(false);
                    }
                    for (before, after) in before.iter().zip(after) {
                        if !self.equal(before, after)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                (Value::Object(before), Value::Object(after)) => {
                    if before.len() != after.len() {
                        return Ok(false);
                    }
                    for (key, before) in before {
                        let Some(after) = after.get(key) else {
                            return Ok(false);
                        };
                        if !self.equal(before, after)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                _ => Ok(crate::collections::equal(before, after)),
            }
        }
        fn visit(
            &mut self,
            before: Option<&Value>,
            after: Option<&Value>,
            path: &str,
            depth: usize,
        ) -> Result<()> {
            if depth > 64 {
                return Err(Error::Limit("mutation diff path exceeds 64 steps".into()));
            }
            if self.work == 0 {
                return Err(Error::Limit("mutation diff exceeds 1000000 steps".into()));
            }
            self.work -= 1;
            match (before, after) {
                (None, Some(value)) => self.change("add", path, Some(value)),
                (Some(_), None) => self.change("remove", path, None),
                (Some(Value::Object(before)), Some(Value::Object(after))) => {
                    self.objects(before, after, path, depth)
                }
                (Some(before), Some(after)) => {
                    if self.equal(before, after)? {
                        Ok(())
                    } else {
                        self.change("replace", path, Some(after))
                    }
                }
                _ => Ok(()),
            }
        }
        fn objects(
            &mut self,
            before: &Document,
            after: &Document,
            path: &str,
            depth: usize,
        ) -> Result<()> {
            let count = before.len().saturating_add(after.len());
            self.work = self
                .work
                .checked_sub(count)
                .ok_or_else(|| Error::Limit("mutation diff exceeds 1000000 steps".into()))?;
            let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
            for key in keys {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                self.visit(before.get(key), after.get(key), &path, depth + 1)?;
            }
            Ok(())
        }
    }
    let mut diff = Diff {
        changes: Vec::new(),
        bytes: crate::links::FetchBudget {
            used: 0,
            limit: 64 * 1024 * 1024,
        },
        work: 1_000_000,
    };
    for document in before.into_iter().chain(after) {
        serde_json::to_writer(&mut diff.bytes, document)
            .map_err(|_| Error::Limit("mutation diff input/output exceeds 64 MiB".into()))?;
    }
    match (before, after) {
        (Some(before), Some(after)) => diff.objects(before, after, "", 0)?,
        (None, Some(after)) => diff.change("add", "", Some(&Value::Object(after.clone())))?,
        (Some(_), None) => diff.change("remove", "", None)?,
        (None, None) => (),
    }
    Ok(Value::Array(diff.changes))
}

pub(crate) fn needs_before(projection: &str) -> Result<bool> {
    has_calls(projection, &["before", "diff"])
}

pub(crate) fn reject_outside_returning(sql: &str) -> Result<()> {
    if has_calls(sql, &["before", "after", "diff"])? {
        return Err(Error::Validation(
            "mutation snapshot helpers require collection RETURNING".into(),
        ));
    }
    Ok(())
}

fn has_calls(sql: &str, names: &[&str]) -> Result<bool> {
    if !sql
        .as_bytes()
        .windows(3)
        .any(|part| part.eq_ignore_ascii_case(b"doc"))
    {
        return Ok(false);
    }
    let tokens = romeql_parser::tokenize(sql)?;
    for (index, token) in tokens.iter().enumerate() {
        let name = token.text.to_ascii_lowercase();
        if matches!(
            token.kind,
            romeql_parser::Kind::Word | romeql_parser::Kind::Identifier
        ) && name
            .strip_prefix("__romedb_h_doc_")
            .is_some_and(|name| names.contains(&name))
            && tokens.get(index + 1).is_some_and(|t| t.text == "(")
        {
            return Ok(true);
        }
        if token.kind == romeql_parser::Kind::Word
            && name == "doc"
            && tokens.get(index + 1).is_some_and(|t| t.text == ":")
            && tokens.get(index + 2).is_some_and(|t| t.text == ":")
            && tokens.get(index + 3).is_some_and(|t| {
                t.kind == romeql_parser::Kind::Word
                    && names.contains(&t.text.to_ascii_lowercase().as_str())
            })
            && tokens.get(index + 4).is_some_and(|t| t.text == "(")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn call_diff(arguments: &[Value]) -> Result<Value> {
    fn document(value: &Value) -> Result<Option<&Document>> {
        match value {
            Value::Null => Ok(None),
            Value::Object(document) => Ok(Some(document)),
            _ => Err(Error::Validation(
                "mutation snapshots must be documents or null".into(),
            )),
        }
    }
    match arguments {
        [before, after] => diff(document(before)?, document(after)?),
        _ => Err(Error::Validation("invalid mutation diff arguments".into())),
    }
}

pub(crate) fn rewrite_expression(
    expr: &mut turso_parser::ast::Expr,
    before_name: &str,
    after_name: &str,
) -> Result<bool> {
    use turso_parser::ast::{Cmd, Expr, OneSelect, ResultColumn, Stmt};
    let mut changed = false;
    let mut failure = None;
    turso_core::walk_expr_mut(expr, &mut |value| {
        let Expr::FunctionCall {
            name,
            args,
            distinctness,
            filter_over,
            order_by,
            within_group,
        } = value
        else {
            return Ok(turso_core::WalkControl::Continue);
        };
        let sql = match name.as_str() {
            "__romedb_h_doc_before" => before_name.to_owned(),
            "__romedb_h_doc_after" => after_name.to_owned(),
            "__romedb_h_doc_diff" => {
                format!("__romedb_h_mutation_diff({before_name},{after_name})")
            }
            _ => return Ok(turso_core::WalkControl::Continue),
        };
        if !args.is_empty()
            || distinctness.is_some()
            || filter_over.filter_clause.is_some()
            || filter_over.over_clause.is_some()
            || !order_by.is_empty()
            || !within_group.is_empty()
        {
            failure = Some(Error::Validation(
                "mutation snapshot helpers require zero arguments and no modifiers".into(),
            ));
            return Ok(turso_core::WalkControl::SkipChildren);
        }
        let replacement = (|| -> Result<Expr> {
            let Cmd::Stmt(Stmt::Select(select)) = crate::select::parsed(&format!("SELECT {sql}"))?
            else {
                return Err(Error::Storage(
                    "invalid generated mutation projection".into(),
                ));
            };
            let OneSelect::Select { mut columns, .. } = select.body.select else {
                return Err(Error::Storage(
                    "invalid generated mutation expression".into(),
                ));
            };
            let ResultColumn::Expr(expr, _) = columns.remove(0) else {
                return Err(Error::Storage("invalid generated mutation column".into()));
            };
            Ok(*expr)
        })();
        match replacement {
            Ok(replacement) => {
                *value = replacement;
                changed = true;
            }
            Err(error) => failure = Some(error),
        }
        Ok(turso_core::WalkControl::SkipChildren)
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_limits_bound_pointer_work_and_input_before_output_allocation() {
        let before = Document::from([("x".repeat(16_385), Value::Integer(1))]);
        let after = Document::from([("x".repeat(16_385), Value::Integer(2))]);
        assert_eq!(
            diff(Some(&before), Some(&after)).unwrap_err().code(),
            "FDB_LIMIT"
        );
        let before = Document::from([("items".into(), Value::Array(vec![Value::Null; 1_000_001]))]);
        assert_eq!(
            diff(Some(&before), Some(&before)).unwrap_err().code(),
            "FDB_LIMIT"
        );
        let before = Document::from([("text".into(), Value::String("x".repeat(32 * 1024 * 1024)))]);
        assert_eq!(
            diff(Some(&before), Some(&before)).unwrap_err().code(),
            "FDB_LIMIT"
        );
    }

    #[test]
    fn diffs_keep_missing_null_pointer_escapes_and_typed_array_values() {
        let before = Document::from([
            ("gone".into(), Value::Null),
            (
                "obj".into(),
                Value::Object(Document::from([("a/b~c".into(), Value::Integer(1))])),
            ),
            ("list".into(), Value::Array(vec![Value::Integer(1)])),
        ]);
        let after = Document::from([
            ("added".into(), Value::Null),
            (
                "obj".into(),
                Value::Object(Document::from([("a/b~c".into(), Value::Integer(2))])),
            ),
            ("list".into(), Value::Array(vec![Value::Number(1.0)])),
        ]);
        let changes = diff(Some(&before), Some(&after)).unwrap();
        let Value::Array(changes) = changes else {
            panic!()
        };
        let paths: Vec<_> = changes
            .iter()
            .map(|v| {
                let Value::Object(change) = v else { panic!() };
                change["path"].clone()
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                Value::String("/added".into()),
                Value::String("/gone".into()),
                Value::String("/list".into()),
                Value::String("/obj/a~1b~0c".into())
            ]
        );
        let Value::Object(added) = &changes[0] else {
            panic!()
        };
        assert_eq!(added["value"], Value::Null);
        let Value::Object(removed) = &changes[1] else {
            panic!()
        };
        assert!(!removed.contains_key("value"));
        let Value::Object(replaced) = &changes[2] else {
            panic!()
        };
        assert_eq!(replaced["value"], Value::Array(vec![Value::Number(1.0)]));
        assert_eq!(
            diff(Some(&before), Some(&before)).unwrap(),
            Value::Array(Vec::new())
        );
    }
    #[test]
    fn creation_deletion_and_typed_reference_identity_are_explicit() {
        let before = Document::from([(
            "ref".into(),
            Value::Record(crate::Record {
                table: "Docs".into(),
                key: crate::Key::String("a".into()),
            }),
        )]);
        let after = Document::from([(
            "ref".into(),
            Value::Record(crate::Record {
                table: "docs".into(),
                key: crate::Key::String("a".into()),
            }),
        )]);
        assert_eq!(
            diff(Some(&before), Some(&after)).unwrap(),
            Value::Array(Vec::new())
        );
        for (old, new, op) in [(None, Some(&after), "add"), (Some(&before), None, "remove")] {
            let Value::Array(changes) = diff(old, new).unwrap() else {
                panic!()
            };
            let Value::Object(change) = &changes[0] else {
                panic!()
            };
            assert_eq!(change["op"], Value::String(op.into()));
            assert_eq!(change["path"], Value::String(String::new()));
        }
    }
}
