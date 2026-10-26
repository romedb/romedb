//! Step predicates: field comparisons combined with AND, OR and NOT.
//! Subexpressions without field references are evaluated once before traversal.
use crate::{Connection, Document, Error, Parameters, Result, Value};
use romeql_parser::Expr;
use std::cmp::Ordering;

#[derive(Debug)]
pub(super) enum Predicate {
    Constant(Value),
    Field(Vec<String>),
    Not(Box<Predicate>),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
    Compare(Box<Predicate>, String, Box<Predicate>),
}

pub(super) fn parameters(expr: &Expr, names: &mut std::collections::BTreeSet<String>) {
    match expr {
        Expr::Parameter(name) => {
            names.insert(name.clone());
        }
        Expr::Case {
            base,
            branches,
            fallback,
        } => {
            for expr in base.iter().chain(fallback.iter()) {
                parameters(expr, names);
            }
            for (a, b) in branches {
                parameters(a, names);
                parameters(b, names);
            }
        }
        Expr::Call(_, args) | Expr::Array(args) => {
            for arg in args {
                parameters(arg, names);
            }
        }
        Expr::Object(fields) => {
            for value in fields.values() {
                parameters(value, names);
            }
        }
        Expr::Unary(_, a) => parameters(a, names),
        Expr::Binary(a, _, b) => {
            parameters(a, names);
            parameters(b, names);
        }
        _ => {}
    }
}

fn has_field(expr: &Expr) -> bool {
    match expr {
        Expr::Field(_) => true,
        Expr::Case {
            base,
            branches,
            fallback,
        } => {
            base.as_deref().is_some_and(has_field)
                || fallback.as_deref().is_some_and(has_field)
                || branches.iter().any(|(a, b)| has_field(a) || has_field(b))
        }
        Expr::Call(_, args) | Expr::Array(args) => args.iter().any(has_field),
        Expr::Object(fields) => fields.values().any(has_field),
        Expr::Unary(_, a) => has_field(a),
        Expr::Binary(a, _, b) => has_field(a) || has_field(b),
        _ => false,
    }
}

fn unsupported() -> Error {
    Error::Unsupported(
        "graph step predicates compare stored fields with values using =, <>, <, <=, >, >=, IS, AND, OR and NOT".into(),
    )
}

impl Predicate {
    pub(super) fn compile(
        connection: &Connection,
        expr: Expr,
        names: &[&str],
        params: &Parameters,
    ) -> Result<Self> {
        if !has_field(&expr) {
            return connection
                .evaluate(expr, params, None)
                .map(Predicate::Constant);
        }
        Ok(match expr {
            Expr::Field(mut path) => {
                if path.len() > 1 && names.iter().any(|name| path[0].eq_ignore_ascii_case(name)) {
                    path.remove(0);
                }
                Predicate::Field(path)
            }
            Expr::Unary(op, a) if op == "NOT" => {
                Predicate::Not(Box::new(Self::compile(connection, *a, names, params)?))
            }
            Expr::Binary(a, op, b) => {
                let a = Box::new(Self::compile(connection, *a, names, params)?);
                let b = Box::new(Self::compile(connection, *b, names, params)?);
                match op.as_str() {
                    "AND" => Predicate::And(a, b),
                    "OR" => Predicate::Or(a, b),
                    "=" | "==" | "!=" | "<>" | "<" | "<=" | ">" | ">=" | "IS" | "IS NOT" => {
                        Predicate::Compare(a, op, b)
                    }
                    _ => return Err(unsupported()),
                }
            }
            _ => return Err(unsupported()),
        })
    }

    pub(super) fn matches(&self, doc: &Document) -> Result<bool> {
        Ok(truth(&self.value(doc)?) == Some(true))
    }

    fn value(&self, doc: &Document) -> Result<Value> {
        Ok(match self {
            Predicate::Constant(value) => value.clone(),
            Predicate::Field(path) => match crate::path_value(doc, path) {
                Ok(value) => value.cloned().unwrap_or(Value::Null),
                Err(Error::Validation(_)) => Value::Null,
                Err(error) => return Err(error),
            },
            Predicate::Not(a) => match truth(&a.value(doc)?) {
                None => Value::Null,
                Some(value) => Value::Boolean(!value),
            },
            Predicate::And(a, b) => {
                let a = truth(&a.value(doc)?);
                if a == Some(false) {
                    return Ok(Value::Boolean(false));
                }
                match (a, truth(&b.value(doc)?)) {
                    (_, Some(false)) => Value::Boolean(false),
                    (Some(true), Some(true)) => Value::Boolean(true),
                    _ => Value::Null,
                }
            }
            Predicate::Or(a, b) => {
                let a = truth(&a.value(doc)?);
                if a == Some(true) {
                    return Ok(Value::Boolean(true));
                }
                match (a, truth(&b.value(doc)?)) {
                    (_, Some(true)) => Value::Boolean(true),
                    (Some(false), Some(false)) => Value::Boolean(false),
                    _ => Value::Null,
                }
            }
            Predicate::Compare(a, op, b) => compare(&a.value(doc)?, op, &b.value(doc)?)?,
        })
    }
}

fn truth(value: &Value) -> Option<bool> {
    match value {
        Value::Null => None,
        Value::Boolean(value) => Some(*value),
        Value::Integer(value) => Some(*value != 0),
        Value::Number(value) => Some(*value != 0.0),
        Value::String(value) => Some(value.trim().parse::<f64>().is_ok_and(|n| n != 0.0)),
        _ => Some(false),
    }
}

fn compare(a: &Value, op: &str, b: &Value) -> Result<Value> {
    if matches!(op, "IS" | "IS NOT") {
        let same = match (a, b) {
            (Value::Null, Value::Null) => true,
            (Value::Null, _) | (_, Value::Null) => false,
            _ => order(a, b)? == Ordering::Equal,
        };
        return Ok(Value::Boolean(same == (op == "IS")));
    }
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(Value::Null);
    }
    let ordering = order(a, b)?;
    Ok(Value::Boolean(match op {
        "=" | "==" => ordering == Ordering::Equal,
        "!=" | "<>" => ordering != Ordering::Equal,
        "<" => ordering == Ordering::Less,
        "<=" => ordering != Ordering::Greater,
        ">" => ordering == Ordering::Greater,
        _ => ordering != Ordering::Less,
    }))
}

/// SQLite storage-class order: numbers, then text, then blobs (records and binary).
fn order(a: &Value, b: &Value) -> Result<Ordering> {
    fn class(value: &Value) -> Result<u8> {
        Ok(match value {
            Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => 0,
            Value::String(_) => 1,
            Value::Binary(_) | Value::Record(_) | Value::Vector(_) => 2,
            _ => {
                return Err(Error::Validation(
                    "graph step predicates compare scalar or record values".into(),
                ))
            }
        })
    }
    let (left, right) = (class(a)?, class(b)?);
    if left != right {
        return Ok(left.cmp(&right));
    }
    Ok(match (a, b) {
        (Value::String(a), Value::String(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Value::Binary(a), Value::Binary(b)) | (Value::Vector(a), Value::Vector(b)) => a.cmp(b),
        (Value::Record(_), Value::Record(_)) => a.encode()?.cmp(&b.encode()?),
        (Value::Binary(_) | Value::Vector(_), _) | (_, Value::Binary(_) | Value::Vector(_)) => {
            a.encode()?.cmp(&b.encode()?)
        }
        _ => {
            let number = |value: &Value| match value {
                Value::Boolean(v) => (i64::from(*v), None),
                Value::Integer(v) => (*v, None),
                Value::Number(v) => (0, Some(*v)),
                _ => unreachable!("numeric class"),
            };
            match (number(a), number(b)) {
                ((a, None), (b, None)) => a.cmp(&b),
                ((_, Some(a)), (_, Some(b))) => a.total_cmp(&b),
                ((a, None), (_, Some(b))) => compare_integer_float(a, b),
                ((_, Some(a)), (b, None)) => compare_integer_float(b, a).reverse(),
            }
        }
    })
}

fn compare_integer_float(integer: i64, float: f64) -> Ordering {
    if float >= i64::MAX as f64 {
        return Ordering::Less;
    }
    if float < i64::MIN as f64 {
        return Ordering::Greater;
    }
    let truncated = float.trunc() as i64;
    match integer.cmp(&truncated) {
        Ordering::Equal => 0.0f64.total_cmp(&(float - float.trunc())),
        other => other,
    }
}
