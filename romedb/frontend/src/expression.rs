//! Typed document expressions. Scalar work is delegated to the pinned engine.
use crate::{Connection, Document, Error, Key, Parameters, Record, Result, Value};
use romeql_parser::Expr;
use std::num::NonZeroUsize;
impl Connection {
    pub(crate) fn evaluate(
        &self,
        expr: Expr,
        params: &Parameters,
        doc: Option<&Document>,
    ) -> Result<Value> {
        self.evaluate_with_budget(expr, params, doc, None)
    }
    pub(crate) fn evaluate_with_budget(
        &self,
        expr: Expr,
        params: &Parameters,
        doc: Option<&Document>,
        mut budget: Option<&mut crate::links::FetchBudget>,
    ) -> Result<Value> {
        let metered = budget.is_some();
        let value = match expr {
            Expr::Case {
                base,
                branches,
                fallback,
            } => {
                let base = base
                    .map(|expr| {
                        self.evaluate_with_budget(*expr, params, doc, budget.as_deref_mut())
                    })
                    .transpose()?;
                let mut chosen = fallback.map(|e| *e);
                for (condition, value) in branches {
                    let condition =
                        self.evaluate_with_budget(condition, params, doc, budget.as_deref_mut())?;
                    let condition = if let Some(base) = &base {
                        self.binary_document(base.clone(), "=", condition, metered)?
                    } else {
                        condition
                    };
                    if self.scalar_expression(
                        "CASE WHEN ?1 THEN 1 ELSE 0 END",
                        &[condition],
                        metered,
                    )? == Value::Integer(1)
                    {
                        chosen = Some(value);
                        break;
                    }
                }
                chosen
                    .map(|e| self.evaluate_with_budget(e, params, doc, budget.as_deref_mut()))
                    .transpose()?
                    .unwrap_or(Value::Null)
            }
            Expr::Null => Value::Null,
            Expr::Boolean(v) => Value::Boolean(v),
            Expr::Integer(v) => Value::Integer(v),
            Expr::Number(v) => Value::Number(v),
            Expr::String(v) => Value::String(v),
            Expr::Record(v) => Value::Record(v),
            Expr::Parameter(name) => params.get(&name).cloned().ok_or(Error::Parameter(name))?,
            Expr::Field(path) => {
                let doc = doc.ok_or_else(|| {
                    let field = path.join(".");
                    Error::Validation(format!(
                        "\"{field}\" is a field reference, but this statement has no source document; use '{field}' for a string"
                    ))
                })?;
                match crate::path_value(doc, &path) {
                    Ok(v) => v.cloned().unwrap_or(Value::Null),
                    Err(Error::Validation(_)) => Value::Null,
                    Err(e) => return Err(e),
                }
            }
            Expr::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(k, e)| {
                        Ok((
                            k,
                            self.evaluate_with_budget(e, params, doc, budget.as_deref_mut())?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            ),
            Expr::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|e| self.evaluate_with_budget(e, params, doc, budget.as_deref_mut()))
                    .collect::<Result<_>>()?,
            ),
            Expr::Unary(op, expr) => {
                let value = self.evaluate_with_budget(*expr, params, doc, budget.as_deref_mut())?;
                if !matches!(op.as_str(), "+" | "-" | "NOT") {
                    return Err(Error::Unsupported("unary expression".into()));
                }
                self.scalar_expression(&format!("{op} ?1"), &[value], metered)?
            }
            Expr::Binary(a, op, b) => {
                if !matches!(
                    op.as_str(),
                    "+" | "-"
                        | "*"
                        | "/"
                        | "%"
                        | "||"
                        | "&"
                        | "|"
                        | "<<"
                        | ">>"
                        | "="
                        | "=="
                        | "!="
                        | "<>"
                        | "<"
                        | "<="
                        | ">"
                        | ">="
                        | "IS"
                        | "IS NOT"
                        | "AND"
                        | "OR"
                ) {
                    return Err(Error::Unsupported("binary expression".into()));
                }
                let a = self.evaluate_with_budget(*a, params, doc, budget.as_deref_mut())?;
                let b = self.evaluate_with_budget(*b, params, doc, budget.as_deref_mut())?;
                self.binary_document(a, &op, b, metered)?
            }
            Expr::Call(name, args) => {
                if name.eq_ignore_ascii_case("coalesce") || name.eq_ignore_ascii_case("ifnull") {
                    if args.len() < 2 || (name.eq_ignore_ascii_case("ifnull") && args.len() != 2) {
                        return Err(Error::Validation("invalid null-helper arity".into()));
                    }
                    let mut result = Value::Null;
                    for expr in args {
                        result =
                            self.evaluate_with_budget(expr, params, doc, budget.as_deref_mut())?;
                        if !matches!(result, Value::Null) {
                            break;
                        }
                    }
                    result
                } else {
                    let args = args
                        .into_iter()
                        .map(|e| self.evaluate_with_budget(e, params, doc, budget.as_deref_mut()))
                        .collect::<Result<Vec<_>>>()?;
                    if name.eq_ignore_ascii_case("replace") {
                        if let Some(budget) = budget.as_deref() {
                            if args
                                .iter()
                                .any(|value| !matches!(value, Value::String(_) | Value::Null))
                            {
                                return Err(Error::Validation(
                                    "computed replace requires strings or null".into(),
                                ));
                            }
                            if let [Value::String(input), Value::String(from), Value::String(to)] =
                                args.as_slice()
                            {
                                if !from.is_empty() && to.len() > from.len() {
                                    let size = input
                                        .matches(from.as_str())
                                        .count()
                                        .checked_mul(to.len() - from.len())
                                        .and_then(|extra| input.len().checked_add(extra));
                                    if size.is_none_or(|size| {
                                        size > budget.limit.saturating_sub(budget.used)
                                    }) {
                                        return Err(Error::Limit(
                                            "computed replace output exceeds evaluation budget"
                                                .into(),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    self.call_document_function(&name, args, metered)?
                }
            }
        };
        value.validate()?;
        if let Some(budget) = budget {
            budget.charge(&value)?;
        }
        Ok(value)
    }
    fn scalar_expression(&self, sql: &str, values: &[Value], metered: bool) -> Result<Value> {
        if values.iter().any(|v| {
            matches!(
                v,
                Value::Record(_) | Value::Object(_) | Value::Array(_) | Value::Vector(_)
            )
        }) {
            return Err(Error::Validation(
                "this scalar operation requires SQL scalar values".into(),
            ));
        }
        let mut statement = self.prepare(format!("SELECT {sql}"))?;
        for (i, value) in values.iter().enumerate() {
            statement.bind_at(
                NonZeroUsize::new(i + 1).expect("one-based binding"),
                crate::scalar(value)?,
            )?;
        }
        let rows = if metered {
            self.meter_statement(&mut statement, crate::collect_rows)?
        } else {
            crate::collect_rows(&mut statement)?
        };
        let value = rows
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next())
            .ok_or_else(|| Error::Storage("expression returned no value".into()))?;
        Ok(crate::from_engine(value))
    }
    fn binary_document(&self, a: Value, op: &str, b: Value, metered: bool) -> Result<Value> {
        if matches!(&a, Value::Record(_)) || matches!(&b, Value::Record(_)) {
            if !matches!(
                op,
                "=" | "==" | "<>" | "!=" | "IS" | "IS NOT" | "<" | "<=" | ">" | ">="
            ) {
                return Err(Error::Validation("record arithmetic is unsupported".into()));
            }
            if matches!(&a, Value::Null) || matches!(&b, Value::Null) {
                return Ok(match op {
                    "IS" => Value::Integer(0),
                    "IS NOT" => Value::Integer(1),
                    _ => Value::Null,
                });
            }
            let cmp = match (&a, &b) {
                (Value::Record(a), Value::Record(b)) => {
                    let table = a
                        .table
                        .to_ascii_lowercase()
                        .cmp(&b.table.to_ascii_lowercase());
                    table.then_with(|| match (&a.key, &b.key) {
                        (Key::Integer(a), Key::Integer(b)) => a.cmp(b),
                        (Key::String(a), Key::String(b)) => a.cmp(b),
                        (Key::Integer(_), Key::String(_)) => std::cmp::Ordering::Less,
                        _ => std::cmp::Ordering::Greater,
                    })
                }
                _ => {
                    return match op {
                        "=" | "==" | "IS" => Ok(Value::Integer(0)),
                        "<>" | "!=" | "IS NOT" => Ok(Value::Integer(1)),
                        _ => Err(Error::Validation(
                            "mixed record/scalar ordering is unsupported".into(),
                        )),
                    }
                }
            };
            let cmp = match cmp {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            return self.scalar_expression(
                &format!("?1 {op} ?2"),
                &[Value::Integer(cmp), Value::Integer(0)],
                metered,
            );
        }
        self.scalar_expression(&format!("?1 {op} ?2"), &[a, b], metered)
    }
    fn call_document_function(
        &self,
        name: &str,
        mut args: Vec<Value>,
        metered: bool,
    ) -> Result<Value> {
        if name.eq_ignore_ascii_case("search::analyze") {
            return match args.as_slice() {
                [Value::String(index), Value::String(input)] => self.analyze_text(index, input),
                [Value::String(index), Value::Null] => {
                    self.analyzer_options(index)?;
                    Ok(Value::Null)
                }
                _ => Err(Error::Validation(
                    "search::analyze expects an index name and text".into(),
                )),
            };
        }
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "vector32"
                | "vector64"
                | "vector32_sparse"
                | "vector8"
                | "vector1bit"
                | "vector_concat"
                | "vector_slice"
                | "vector_distance_cos"
                | "vector_distance_l2"
                | "vector_distance_jaccard"
                | "vector_distance_dot"
                | "vector_extract"
        ) {
            for arg in &mut args {
                if let Value::Vector(bytes) | Value::Binary(bytes) = arg {
                    crate::vectors::dimensions(bytes)?;
                    *arg = Value::Binary(bytes.clone());
                }
            }
            let slots = (1..=args.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let native = if name.eq_ignore_ascii_case("vector_concat") {
                "__romedb_vector_concat"
            } else {
                name
            };
            let value = self.scalar_expression(
                &format!("{}({slots})", crate::quote(native)),
                &args,
                metered,
            )?;
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "vector32"
                    | "vector64"
                    | "vector32_sparse"
                    | "vector8"
                    | "vector1bit"
                    | "vector_concat"
                    | "vector_slice"
            ) {
                let Value::Binary(bytes) = value else {
                    return Err(Error::Storage("vector constructor result".into()));
                };
                let value = Value::Vector(bytes);
                value.validate()?;
                return Ok(value);
            }
            return Ok(value);
        }
        if let Some(helper) = crate::spatial::helper_name(name) {
            return crate::spatial::call(helper, &args);
        }
        match name.to_ascii_lowercase().as_str() {
            "string::slugify" => crate::bundled::call("slugify", &args),
            "string::normalize" => crate::bundled::call("normalize", &args),
            "type::record" => match args.as_slice() {
                [Value::String(table), key] => {
                    let key = match key {
                        Value::String(s) => Key::String(s.clone()),
                        Value::Integer(i) => Key::Integer(*i),
                        _ => {
                            return Err(Error::Validation(
                                "record key must be string or int64".into(),
                            ))
                        }
                    };
                    Ok(Value::Record(Record {
                        table: crate::canonical(table)?,
                        key,
                    }))
                }
                _ => Err(Error::Validation(
                    "type::record expects target string and key".into(),
                )),
            },
            "record::id" | "record::table" => match args.as_slice() {
                [Value::Record(r)] => {
                    if name.eq_ignore_ascii_case("record::table") {
                        Ok(Value::String(r.table.to_ascii_lowercase()))
                    } else {
                        Ok(match &r.key {
                            Key::String(s) => Value::String(s.clone()),
                            Key::Integer(i) => Value::Integer(*i),
                        })
                    }
                }
                _ => Err(Error::Validation(
                    "record extractor expects one typed record".into(),
                )),
            },
            "array::new" => Ok(Value::Array(args)),
            "array::contains" | "array::len" | "array::distinct" | "array::flatten"
            | "doc::keys" | "doc::values" | "doc::entries" | "doc::from_entries" => {
                crate::collections::call(
                    &name
                        .to_ascii_lowercase()
                        .replace("doc::", "object_")
                        .replace("::", "_"),
                    &args,
                )
            }
            "array::append" => {
                if args.len() != 2 {
                    return Err(Error::Validation(
                        "array::append expects array and element".into(),
                    ));
                }
                let element = args.pop().expect("two arguments");
                let Value::Array(mut values) = args.pop().expect("first argument") else {
                    return Err(Error::Validation(
                        "array::append requires an existing array".into(),
                    ));
                };
                values.push(element);
                Ok(Value::Array(values))
            }
            "doc::get" | "doc::has" => match args.as_slice() {
                [value, Value::String(path)] => {
                    let found = crate::path::get(value, path)?;
                    if name.eq_ignore_ascii_case("doc::has") {
                        Ok(Value::Boolean(found.is_some()))
                    } else {
                        Ok(found.cloned().unwrap_or(Value::Null))
                    }
                }
                _ => Err(Error::Validation(
                    "document path function expects value and path string".into(),
                )),
            },
            _ => {
                if name.contains("::") {
                    return self.call_user_function(name, &args);
                }
                if name.to_ascii_lowercase().starts_with("__romedb_") {
                    return Err(Error::Unsupported(format!("document function {name}")));
                }
                let args_sql = (1..=args.len())
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(",");
                self.scalar_expression(
                    &format!("{}({args_sql})", crate::quote(name)),
                    &args,
                    metered,
                )
            }
        }
    }
}
