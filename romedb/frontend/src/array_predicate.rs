use crate::{Error, Parameters, Result, Value};
use romeql_parser::Expr;
use std::collections::BTreeSet;

pub(crate) struct Predicate {
    expression: Expr,
    pub(crate) parameters: BTreeSet<String>,
    pub(crate) nodes: usize,
}

impl Predicate {
    pub(crate) fn prepare(source: &str) -> Result<Self> {
        if source.len() > 16_384 {
            return Err(Error::Limit("array predicate exceeds 16384 bytes".into()));
        }
        let expression = romeql_parser::parse_expression(source)?;
        let mut predicate = Self {
            expression,
            parameters: BTreeSet::new(),
            nodes: 0,
        };
        validate(
            &predicate.expression,
            &mut predicate.parameters,
            &mut predicate.nodes,
        )?;
        Ok(predicate)
    }

    pub(crate) fn matches(
        &self,
        value: &Value,
        parameters: &Parameters,
        budget: &mut crate::links::FetchBudget,
    ) -> Result<bool> {
        Ok(truth(&evaluate(&self.expression, value, parameters, budget)?)? == Some(true))
    }
}

fn validate(expression: &Expr, parameters: &mut BTreeSet<String>, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if *nodes > 256 {
        return Err(Error::Limit(
            "array predicate exceeds 256 expression nodes".into(),
        ));
    }
    match expression {
        Expr::Unary(op, value) if op == "NOT" => validate(value, parameters, nodes)?,
        Expr::Binary(left, op, right)
            if matches!(
                op.as_str(),
                "=" | "==" | "!=" | "<>" | "<" | "<=" | ">" | ">=" | "IS" | "IS NOT" | "AND" | "OR"
            ) =>
        {
            validate(left, parameters, nodes)?;
            validate(right, parameters, nodes)?;
        }
        Expr::Parameter(name) if name.starts_with('$') || name.starts_with('@') => {
            parameters.insert(name.clone());
        }
        Expr::Field(path) if path.len() <= 64 => {
            *nodes += path.len();
        }
        Expr::Null
        | Expr::Boolean(_)
        | Expr::Integer(_)
        | Expr::Number(_)
        | Expr::String(_)
        | Expr::Record(_) => {}
        _ => {
            return Err(Error::Unsupported(
                "array predicates accept field comparisons, named parameters, NOT, AND and OR"
                    .into(),
            ))
        }
    }
    if *nodes > 256 {
        return Err(Error::Limit(
            "array predicate exceeds 256 expression/path nodes".into(),
        ));
    }
    Ok(())
}

fn truth(value: &Value) -> Result<Option<bool>> {
    Ok(turso_core::Numeric::from_value(&crate::scalar(value)?).map(|number| number.to_bool()))
}

fn boolean(value: Option<bool>) -> Value {
    value.map_or(Value::Null, Value::Boolean)
}

fn leaf(value: &Value, budget: &mut crate::links::FetchBudget) -> Result<Value> {
    budget
        .charge(value)
        .map_err(|_| Error::Limit("array predicate evaluated values exceed 64 MiB".into()))?;
    Ok(value.clone())
}

fn evaluate(
    expression: &Expr,
    element: &Value,
    parameters: &Parameters,
    budget: &mut crate::links::FetchBudget,
) -> Result<Value> {
    match expression {
        Expr::Field(path) => {
            let mut path = path.as_slice();
            if path
                .first()
                .is_some_and(|key| key.eq_ignore_ascii_case("this"))
            {
                path = &path[1..]
            }
            let mut value = element;
            for key in path {
                let Value::Object(document) = value else {
                    return Ok(Value::Null);
                };
                let Some(next) = document.get(key) else {
                    return Ok(Value::Null);
                };
                value = next;
            }
            leaf(value, budget)
        }
        Expr::Parameter(name) => leaf(
            parameters
                .get(name)
                .ok_or_else(|| Error::Parameter(name.clone()))?,
            budget,
        ),
        Expr::Null => Ok(Value::Null),
        Expr::Boolean(value) => Ok(Value::Boolean(*value)),
        Expr::Integer(value) => Ok(Value::Integer(*value)),
        Expr::Number(value) => Ok(Value::Number(*value)),
        Expr::String(value) => leaf(&Value::String(value.clone()), budget),
        Expr::Record(value) => leaf(&Value::Record(value.clone()), budget),
        Expr::Unary(_, value) => Ok(boolean(
            truth(&evaluate(value, element, parameters, budget)?)?.map(|value| !value),
        )),
        Expr::Binary(left, op, right) => {
            let left = evaluate(left, element, parameters, budget)?;
            let right = evaluate(right, element, parameters, budget)?;
            if matches!(op.as_str(), "AND" | "OR") {
                let (left, right) = (truth(&left)?, truth(&right)?);
                return Ok(boolean(match (op.as_str(), left, right) {
                    ("AND", Some(false), _) | ("AND", _, Some(false)) => Some(false),
                    ("AND", Some(true), Some(true)) => Some(true),
                    ("OR", Some(true), _) | ("OR", _, Some(true)) => Some(true),
                    ("OR", Some(false), Some(false)) => Some(false),
                    _ => None,
                }));
            }
            if matches!(left, Value::Null) || matches!(right, Value::Null) {
                return Ok(match op.as_str() {
                    "IS" => Value::Boolean(left == right),
                    "IS NOT" => Value::Boolean(left != right),
                    _ => Value::Null,
                });
            }
            let equality = matches!(op.as_str(), "=" | "==" | "IS" | "!=" | "<>" | "IS NOT");
            let mixed_record =
                matches!(left, Value::Record(_)) != matches!(right, Value::Record(_));
            if equality && mixed_record {
                return Ok(Value::Boolean(matches!(
                    op.as_str(),
                    "!=" | "<>" | "IS NOT"
                )));
            }
            let comparison =
                crate::functions::compare_values(&left, &right)?.expect("nonnull inputs");
            Ok(Value::Boolean(match op.as_str() {
                "=" | "==" | "IS" => comparison.is_eq(),
                "!=" | "<>" | "IS NOT" => !comparison.is_eq(),
                "<" => comparison.is_lt(),
                "<=" => !comparison.is_gt(),
                ">" => comparison.is_gt(),
                ">=" => !comparison.is_lt(),
                _ => unreachable!(),
            }))
        }
        _ => unreachable!(),
    }
}
