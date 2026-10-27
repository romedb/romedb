//! Deterministic field CHECKs evaluate the candidate, never the stored row.
use crate::{Collection, Connection, Document, Error, Field, Result, Value};
use std::num::{NonZeroU32, NonZeroUsize};
use turso_core::Value as EngineValue;
use turso_parser::ast::*;

fn invalid(message: impl Into<String>) -> Error {
    Error::Validation(message.into())
}
fn parse_check(sql: &str) -> Result<Expr> {
    // Only the path expander may introduce this marker. It is not a callable
    // CHECK helper and must not bypass the function eligibility checks.
    let tokens = romeql_parser::tokenize(sql)?;
    if tokens
        .windows(2)
        .any(|pair| pair[0].text.eq_ignore_ascii_case("__romedb_path") && pair[1].text == "(")
    {
        return Err(invalid("internal path markers are not CHECK functions"));
    }
    let sql = crate::select::expand_paths(sql)?;
    let Cmd::Stmt(Stmt::Select(select)) = crate::select::parsed(&format!("SELECT ({sql})"))? else {
        return Err(invalid("CHECK must be an expression"));
    };
    if select.with.is_some()
        || !select.body.compounds.is_empty()
        || !select.order_by.is_empty()
        || select.limit.is_some()
    {
        return Err(invalid("CHECK must be an expression"));
    }
    let OneSelect::Select {
        mut columns,
        from: None,
        where_clause: None,
        group_by: None,
        window_clause,
        ..
    } = select.body.select
    else {
        return Err(invalid("CHECK cannot read the database"));
    };
    if columns.len() != 1 || !window_clause.is_empty() {
        return Err(invalid("CHECK must be one expression"));
    }
    let ResultColumn::Expr(expr, _) = columns.remove(0) else {
        return Err(invalid("CHECK must be an expression"));
    };
    Ok(*expr)
}
fn field_path(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            filter_over,
            order_by,
            within_group,
        } if name.as_str() == "__romedb_path"
            && (4..=64).contains(&args.len())
            && distinctness.is_none()
            && filter_over.filter_clause.is_none()
            && filter_over.over_clause.is_none()
            && order_by.is_empty()
            && within_group.is_empty() =>
        {
            args.iter()
                .map(|arg| match arg.as_ref() {
                    Expr::Id(n) | Expr::Name(n) => Some(n.as_str().to_owned()),
                    _ => None,
                })
                .collect()
        }
        Expr::Parenthesized(es) if es.len() == 1 => field_path(&es[0]),
        Expr::Id(n) | Expr::Name(n) => Some(vec![n.as_str().into()]),
        Expr::Qualified(a, b) => Some(vec![a.as_str().into(), b.as_str().into()]),
        Expr::DoublyQualified(a, b, c) => Some(vec![
            a.as_str().into(),
            b.as_str().into(),
            c.as_str().into(),
        ]),
        Expr::FieldAccess { base, field, .. } => {
            let mut path = field_path(base)?;
            path.push(field.as_str().into());
            Some(path)
        }
        _ => None,
    }
}
#[derive(Clone, Copy)]
enum FieldBinding {
    Key,
    Typed,
    Sql,
}

fn bind_field(
    expr: &mut Expr,
    doc: Option<&Document>,
    bindings: &mut Vec<EngineValue>,
    binding: FieldBinding,
) -> Result<bool> {
    if let Some(path) = field_path(expr) {
        let value = match doc {
            Some(doc) => match crate::path_value(doc, &path) {
                Ok(v) => v,
                Err(Error::Validation(_)) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let value = value.unwrap_or(&Value::Null);
        bindings.push(match binding {
            FieldBinding::Typed => EngineValue::Blob(value.encode()?),
            FieldBinding::Key => crate::index_scalar(value)?,
            FieldBinding::Sql => crate::scalar(value)?,
        });
        let index =
            u32::try_from(bindings.len()).map_err(|_| invalid("too many CHECK references"))?;
        *expr = Expr::Variable(Variable::indexed(
            NonZeroU32::new(index).expect("one-based binding"),
        ));
        return Ok(true);
    }
    Ok(false)
}
// Reassociate homogeneous boolean chains without changing operand order.
// Deep left-associated chains exhaust the pinned engine's preparation stack.
fn balance_boolean(expr: &mut Expr) {
    let Expr::Binary(_, op @ (Operator::And | Operator::Or), _) = expr else {
        return;
    };
    let op = *op;
    let mut pending = vec![std::mem::replace(expr, Expr::Literal(Literal::Null))];
    let mut terms = Vec::new();
    while let Some(term) = pending.pop() {
        match term {
            Expr::Binary(left, current, right) if current == op => {
                pending.push(*right);
                pending.push(*left);
            }
            Expr::Parenthesized(mut values)
                if values.len() == 1
                    && matches!(values[0].as_ref(), Expr::Binary(_, current, _) if *current == op) =>
            {
                pending.push(*values.pop().expect("single operand"));
            }
            term => terms.push(term),
        }
    }
    while terms.len() > 1 {
        let mut next = Vec::with_capacity(terms.len().div_ceil(2));
        let mut iter = terms.into_iter();
        while let Some(left) = iter.next() {
            next.push(match iter.next() {
                Some(right) => Expr::Parenthesized(vec![Box::new(Expr::Binary(
                    Box::new(left),
                    op,
                    Box::new(right),
                ))]),
                None => left,
            });
        }
        terms = next;
    }
    *expr = terms.pop().expect("boolean chain contains operands");
}
// Candidate references already have typed keys. Native expression results must
// be packed first so a binary payload cannot impersonate a record key.
fn lower_comparison(
    expr: &mut Expr,
    doc: Option<&Document>,
    bindings: &mut Vec<EngineValue>,
) -> Result<()> {
    if bind_field(expr, doc, bindings, FieldBinding::Key)? {
        return Ok(());
    }
    match expr {
        Expr::Parenthesized(values) => {
            for value in values {
                lower_comparison(value, doc, bindings)?;
            }
            return Ok(());
        }
        Expr::Unary(UnaryOperator::Positive, value) => {
            return lower_comparison(value, doc, bindings)
        }
        Expr::Collate(value, name) => {
            if !matches!(
                name.as_str().to_ascii_lowercase().as_str(),
                "binary" | "nocase" | "rtrim"
            ) {
                return Err(invalid("CHECK requires a built-in collation"));
            }
            return lower_comparison(value, doc, bindings);
        }
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(value) = base {
                lower_comparison(value, doc, bindings)?;
            }
            for (condition, value) in when_then_pairs {
                if base.is_none() {
                    lower_mode(condition, doc, bindings, FieldBinding::Sql)?;
                } else {
                    lower_comparison(condition, doc, bindings)?;
                }
                lower_comparison(value, doc, bindings)?;
            }
            if let Some(value) = else_expr {
                lower_comparison(value, doc, bindings)?;
            }
            return Ok(());
        }
        _ => {}
    }
    let cast_type = match expr {
        Expr::Cast { type_name, .. } => Some(type_name.clone()),
        _ => None,
    };
    // Numeric/text results already have the same SQL and comparison-key form.
    // Avoid deep conversion wrappers in boolean chains prepared by the engine.
    let scalar_result = match expr {
        Expr::Literal(value) => !matches!(value, Literal::Blob(_)),
        Expr::FunctionCall { name, .. } => matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "length"
                | "lower"
                | "upper"
                | "trim"
                | "ltrim"
                | "rtrim"
                | "abs"
                | "round"
                | "typeof"
                | "unicode"
                | "instr"
                | "replace"
        ),
        Expr::Unary(_, _)
        | Expr::IsNull(_)
        | Expr::NotNull(_)
        | Expr::Between { .. }
        | Expr::InList { .. }
        | Expr::Like { .. } => true,
        Expr::Binary(_, op, _) => {
            op.is_comparison()
                || matches!(
                    op,
                    Operator::Add
                        | Operator::Subtract
                        | Operator::Multiply
                        | Operator::Divide
                        | Operator::Modulus
                        | Operator::Concat
                        | Operator::BitwiseAnd
                        | Operator::BitwiseOr
                        | Operator::LeftShift
                        | Operator::RightShift
                        | Operator::And
                        | Operator::Or
                )
        }
        Expr::Cast {
            type_name: Some(t), ..
        } => !t.name.eq_ignore_ascii_case("blob"),
        _ => false,
    };
    lower_mode(expr, doc, bindings, FieldBinding::Sql)?;
    if scalar_result {
        return Ok(());
    }
    *expr = parse_check(&format!("__romedb_unwrap(__romedb_pack({expr}))"))?;
    if let Some(type_name) = cast_type {
        *expr = Expr::Cast {
            expr: Box::new(expr.clone()),
            type_name,
        };
    }
    Ok(())
}
fn typed_range_operand(expr: &Expr) -> bool {
    if field_path(expr).is_some() {
        return true;
    }
    match expr {
        Expr::Literal(
            Literal::Numeric(_) | Literal::String(_) | Literal::Blob(_) | Literal::Null,
        ) => true,
        Expr::Parenthesized(values) if values.len() == 1 => typed_range_operand(&values[0]),
        Expr::Unary(UnaryOperator::Positive | UnaryOperator::Negative, value) => {
            matches!(value.as_ref(), Expr::Literal(Literal::Numeric(_)))
        }
        _ => false,
    }
}
fn lower_range_operand(
    expr: &mut Expr,
    doc: Option<&Document>,
    bindings: &mut Vec<EngineValue>,
) -> Result<()> {
    if !bind_field(expr, doc, bindings, FieldBinding::Typed)? {
        lower_mode(expr, doc, bindings, FieldBinding::Sql)?;
        *expr = parse_check(&format!("__romedb_pack({expr})"))?;
    }
    Ok(())
}
fn lower(expr: &mut Expr, doc: Option<&Document>, bindings: &mut Vec<EngineValue>) -> Result<()> {
    lower_mode(expr, doc, bindings, FieldBinding::Key)
}
fn lower_mode(
    expr: &mut Expr,
    doc: Option<&Document>,
    bindings: &mut Vec<EngineValue>,
    binding: FieldBinding,
) -> Result<()> {
    if bind_field(expr, doc, bindings, binding)? {
        return Ok(());
    }
    match expr {
        Expr::Literal(Literal::CurrentDate | Literal::CurrentTime | Literal::CurrentTimestamp) => {
            return Err(invalid("CHECK cannot depend on current time"))
        }
        Expr::Literal(_) => {}
        Expr::Variable(_) => return Err(invalid("CHECK cannot contain bound parameters")),
        Expr::Binary(a, op, b) => {
            if matches!(
                op,
                Operator::Equals | Operator::NotEquals | Operator::Is | Operator::IsNot
            ) {
                lower_comparison(a, doc, bindings)?;
                lower_comparison(b, doc, bindings)?;
                return Ok(());
            }
            if matches!(
                op,
                Operator::Add
                    | Operator::Subtract
                    | Operator::Multiply
                    | Operator::Divide
                    | Operator::Modulus
                    | Operator::Concat
                    | Operator::ArrowRight
                    | Operator::ArrowRightShift
                    | Operator::BitwiseAnd
                    | Operator::BitwiseOr
                    | Operator::LeftShift
                    | Operator::RightShift
                    | Operator::And
                    | Operator::Or
            ) {
                lower_mode(a, doc, bindings, FieldBinding::Sql)?;
                lower_mode(b, doc, bindings, FieldBinding::Sql)?;
                if matches!(op, Operator::And | Operator::Or) {
                    balance_boolean(expr);
                }
                return Ok(());
            }
            if matches!(
                op,
                Operator::Less | Operator::LessEquals | Operator::Greater | Operator::GreaterEquals
            ) && typed_range_operand(a)
                && typed_range_operand(b)
            {
                lower_range_operand(a, doc, bindings)?;
                lower_range_operand(b, doc, bindings)?;
                *expr = parse_check(&format!("__romedb_compare({a},{b}) {op} 0"))?;
                return Ok(());
            }
            lower(a, doc, bindings)?;
            lower(b, doc, bindings)?;
        }
        Expr::Unary(UnaryOperator::Positive, e) => lower_mode(e, doc, bindings, binding)?,
        Expr::Unary(_, e) => lower_mode(e, doc, bindings, FieldBinding::Sql)?,
        Expr::IsNull(e) | Expr::NotNull(e) => lower(e, doc, bindings)?,
        Expr::Cast { expr: e, type_name } => {
            let Some(t) = type_name else {
                return Err(invalid("CHECK CAST requires a built-in scalar type"));
            };
            if t.size.is_some()
                || t.array_dimensions != 0
                || !matches!(
                    t.name.to_ascii_lowercase().as_str(),
                    "integer" | "int" | "real" | "float" | "double" | "numeric" | "text" | "blob"
                )
            {
                return Err(invalid(
                    "CHECK CAST requires an unsized built-in scalar type",
                ));
            }
            lower_mode(e, doc, bindings, FieldBinding::Sql)?;
        }
        Expr::Collate(e, name) => {
            if !matches!(
                name.as_str().to_ascii_lowercase().as_str(),
                "binary" | "nocase" | "rtrim"
            ) {
                return Err(invalid("CHECK requires a built-in collation"));
            }
            lower_mode(e, doc, bindings, binding)?;
        }
        Expr::Between {
            lhs,
            start,
            end,
            not,
        } => {
            if typed_range_operand(lhs) && typed_range_operand(start) && typed_range_operand(end) {
                lower_range_operand(lhs, doc, bindings)?;
                lower_range_operand(start, doc, bindings)?;
                lower_range_operand(end, doc, bindings)?;
                let negate = if *not { "NOT " } else { "" };
                *expr = parse_check(&format!("{negate}__romedb_between({lhs},{start},{end})"))?;
                return Ok(());
            }
            lower(lhs, doc, bindings)?;
            lower(start, doc, bindings)?;
            lower(end, doc, bindings)?;
        }
        Expr::Like {
            lhs,
            rhs,
            escape,
            op,
            ..
        } => {
            if *op != LikeOperator::Glob {
                return Err(invalid("CHECK supports fixed GLOB matching; LIKE/REGEXP/MATCH eligibility is not defined"));
            }
            lower_mode(lhs, doc, bindings, FieldBinding::Sql)?;
            lower_mode(rhs, doc, bindings, FieldBinding::Sql)?;
            if let Some(e) = escape {
                lower_mode(e, doc, bindings, FieldBinding::Sql)?;
            }
        }
        Expr::InList { lhs, rhs, .. } => {
            lower_comparison(lhs, doc, bindings)?;
            for e in rhs {
                lower_comparison(e, doc, bindings)?;
            }
        }
        Expr::Parenthesized(es) => {
            for e in es {
                lower_mode(e, doc, bindings, binding)?;
            }
        }
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(e) = base {
                lower_comparison(e, doc, bindings)?;
            }
            for (a, b) in when_then_pairs {
                if base.is_none() {
                    lower_mode(a, doc, bindings, FieldBinding::Sql)?;
                } else {
                    lower_comparison(a, doc, bindings)?;
                }
                lower_mode(b, doc, bindings, binding)?;
            }
            if let Some(e) = else_expr {
                lower_mode(e, doc, bindings, binding)?;
            }
        }
        Expr::FunctionCall {
            name,
            args,
            distinctness,
            filter_over,
            order_by,
            within_group,
        } => {
            let name = name.as_str().to_ascii_lowercase();
            let eligible = matches!(
                name.as_str(),
                "length"
                    | "lower"
                    | "upper"
                    | "trim"
                    | "ltrim"
                    | "rtrim"
                    | "substr"
                    | "substring"
                    | "abs"
                    | "round"
                    | "coalesce"
                    | "ifnull"
                    | "nullif"
                    | "typeof"
                    | "unicode"
                    | "instr"
                    | "replace"
            ) || (matches!(name.as_str(), "min" | "max") && args.len() >= 2);
            if !eligible
                || distinctness.is_some()
                || filter_over.filter_clause.is_some()
                || filter_over.over_clause.is_some()
                || !order_by.is_empty()
                || !within_group.is_empty()
            {
                return Err(invalid(format!("function {name} is not eligible in CHECK")));
            }
            for e in args {
                lower_mode(e, doc, bindings, FieldBinding::Sql)?;
            }
        }
        _ => {
            return Err(invalid(
                "CHECK disallows database reads, aggregates, and this expression form",
            ))
        }
    }
    Ok(())
}
impl Connection {
    pub(crate) fn check_definition(&self, field: &Field) -> Result<()> {
        let Some(sql) = &field.check else {
            return Ok(());
        };
        romeql_parser::validate_delimiter_depth(&romeql_parser::tokenize(sql)?)?;
        let mut expr = parse_check(sql)?;
        let mut bindings = Vec::new();
        lower_mode(&mut expr, None, &mut bindings, FieldBinding::Sql)?;
        // Preparation validates supported function signatures even on an empty
        // collection. It never executes an expression or reads a table.
        self.prepare(format!("SELECT CASE WHEN ({expr}) THEN 1 ELSE 0 END"))
            .map_err(|e| invalid(format!("invalid CHECK: {e}")))?;
        Ok(())
    }
    pub(crate) fn validate_candidate(&self, collection: &Collection, doc: &Document) -> Result<()> {
        self.validate_candidate_mode(collection, doc, false)
    }
    pub(crate) fn validate_integrity_candidate(
        &self,
        collection: &Collection,
        doc: &Document,
    ) -> Result<()> {
        self.validate_candidate_mode(collection, doc, true)
    }
    fn validate_candidate_mode(
        &self,
        collection: &Collection,
        doc: &Document,
        preserve_engine_errors: bool,
    ) -> Result<()> {
        crate::validate_document(collection, doc)?;
        if preserve_engine_errors {
            self.validate_computed_fields(collection, doc)?;
        }
        for field in &collection.fields {
            let Some(sql) = &field.check else {
                continue;
            };
            if crate::path_value(doc, &field.path)?.is_none_or(|v| matches!(v, Value::Null)) {
                continue;
            }
            let result = (|| -> Result<bool> {
                let mut expr = parse_check(sql)?;
                let mut bindings = Vec::new();
                lower_mode(&mut expr, Some(doc), &mut bindings, FieldBinding::Sql)?;
                let mut statement =
                    self.prepare(format!("SELECT CASE WHEN ({expr}) THEN 1 ELSE 0 END"))?;
                for (i, value) in bindings.into_iter().enumerate() {
                    statement
                        .bind_at(NonZeroUsize::new(i + 1).expect("one-based binding"), value)?;
                }
                let rows = self.meter_statement(&mut statement, crate::collect_rows)?;
                Ok(matches!(
                    rows.first().and_then(|row| row.first()),
                    Some(EngineValue::Numeric(turso_core::Numeric::Integer(1)))
                ))
            })();
            match result {
                Ok(true) => {}
                Ok(false) => {
                    return Err(invalid(format!(
                        "CHECK failed for field {}",
                        field.path.join(".")
                    )))
                }
                Err(error) => {
                    if preserve_engine_errors && matches!(error, Error::Engine(_)) {
                        return Err(error);
                    }
                    return Err(invalid(format!(
                        "CHECK failed for field {}: {error}",
                        field.path.join(".")
                    )));
                }
            }
        }
        Ok(())
    }
}
