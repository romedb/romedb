use crate::{Collection, Connection, Document, Error, Parameters, Result, Value};
use romeql_parser::Expr;
use std::collections::BTreeSet;

struct Definition {
    expression: Expr,
    dependencies: BTreeSet<Vec<String>>,
}

fn definition(sql: &str) -> Result<Definition> {
    crate::parser_stack(|| definition_inner(sql))
}

fn definition_inner(sql: &str) -> Result<Definition> {
    if sql.len() > 16 * 1024 {
        return Err(Error::Limit("computed expression exceeds 16 KiB".into()));
    }
    let expression = romeql_parser::parse_expression(sql)?;
    let mut dependencies = BTreeSet::new();
    let mut work = 0;
    fn visit(
        expression: &Expr,
        dependencies: &mut BTreeSet<Vec<String>>,
        work: &mut usize,
    ) -> Result<()> {
        *work += 1;
        if *work > 256 {
            return Err(Error::Limit("computed expression exceeds 256 nodes".into()));
        }
        match expression {
            Expr::Parameter(_) => {
                return Err(Error::Validation(
                    "computed fields cannot capture parameters".into(),
                ))
            }
            Expr::Field(path) => {
                crate::validate_path(path)?;
                if path.len() > 64 {
                    return Err(Error::Limit(
                        "computed dependency exceeds 64 path steps".into(),
                    ));
                }
                dependencies.insert(path.clone());
            }
            Expr::Unary(_, value) => visit(value, dependencies, work)?,
            Expr::Binary(left, _, right) => {
                visit(left, dependencies, work)?;
                visit(right, dependencies, work)?;
            }
            Expr::Call(name, arguments) => {
                let arity = arguments.len();
                let allowed = match name.to_ascii_lowercase().as_str() {
                    "abs" | "length" | "lower" | "upper" | "record::id" | "record::table"
                    | "array::len" | "array::distinct" | "array::flatten" | "doc::keys"
                    | "doc::values" | "doc::entries" | "doc::from_entries" | "string::slugify"
                    | "vector32" | "vector64" => arity == 1,
                    "trim" | "ltrim" | "rtrim" | "round" => (1..=2).contains(&arity),
                    "substr" | "substring" => (2..=3).contains(&arity),
                    "replace" | "geo::within" => arity == 3,
                    "coalesce" => arity >= 2,
                    "ifnull" | "nullif" | "type::record" | "doc::get" | "doc::has"
                    | "string::normalize" | "array::append" | "array::contains" | "geo::point"
                    | "geo::distance" => arity == 2,
                    "array::new" => true,
                    _ => false,
                };
                if !allowed {
                    return Err(Error::Validation(format!(
                        "function {name} or its arity is not eligible in a computed field"
                    )));
                }
                for value in arguments {
                    visit(value, dependencies, work)?;
                }
            }
            Expr::Object(fields) => {
                for value in fields.values() {
                    visit(value, dependencies, work)?;
                }
            }
            Expr::Array(values) => {
                for value in values {
                    visit(value, dependencies, work)?;
                }
            }
            Expr::Case {
                base,
                branches,
                fallback,
            } => {
                for value in base.iter().chain(fallback) {
                    visit(value, dependencies, work)?;
                }
                for (condition, value) in branches {
                    visit(condition, dependencies, work)?;
                    visit(value, dependencies, work)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(&expression, &mut dependencies, &mut work)?;
    Ok(Definition {
        expression,
        dependencies,
    })
}

pub(crate) fn validate_options(options: &crate::FieldOptions) -> Result<()> {
    if let Some(sql) = &options.computed {
        if options.default.is_some() || options.readonly {
            return Err(Error::Validation(
                "VALUE cannot be combined with DEFAULT or READONLY".into(),
            ));
        }
        definition(sql)?;
    }
    Ok(())
}

fn plan(collection: &Collection) -> Result<Vec<(Vec<String>, Expr)>> {
    let fields = collection
        .field_policies
        .iter()
        .filter_map(|policy| {
            policy
                .options
                .computed
                .as_ref()
                .map(|sql| (policy.path.clone(), sql))
        })
        .collect::<Vec<_>>();
    if fields.len() > 128 {
        return Err(Error::Limit(
            "collection exceeds 128 computed fields".into(),
        ));
    }
    let definitions = fields
        .iter()
        .map(|(_, sql)| definition(sql))
        .collect::<Result<Vec<_>>>()?;
    let mut edges = vec![Vec::new(); fields.len()];
    for (index, (path, _)) in fields.iter().enumerate() {
        for (other, (dependency, _)) in fields.iter().enumerate() {
            if index != other && (path.starts_with(dependency) || dependency.starts_with(path)) {
                return Err(Error::Validation(
                    "computed output paths cannot overlap".into(),
                ));
            }
            if definitions[index]
                .dependencies
                .iter()
                .any(|input| input.starts_with(dependency) || dependency.starts_with(input))
            {
                edges[index].push(other);
            }
        }
    }
    fn visit(
        index: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<()> {
        match state[index] {
            2 => return Ok(()),
            1 => return Err(Error::Validation("computed field dependency cycle".into())),
            _ => {}
        }
        state[index] = 1;
        for dependency in &edges[index] {
            visit(*dependency, edges, state, order)?;
        }
        state[index] = 2;
        order.push(index);
        Ok(())
    }
    let mut state = vec![0; fields.len()];
    let mut order = Vec::new();
    for index in 0..fields.len() {
        visit(index, &edges, &mut state, &mut order)?;
    }
    Ok(order
        .into_iter()
        .map(|index| {
            (
                fields[index].0.clone(),
                definitions[index].expression.clone(),
            )
        })
        .collect())
}

pub(crate) fn validate_catalog(collection: &Collection) -> Result<()> {
    plan(collection).map(|_| ())
}

impl Connection {
    pub(crate) fn compute_fields(
        &self,
        collection: &Collection,
        document: &mut Document,
    ) -> Result<()> {
        let plan = plan(collection)?;
        if plan.is_empty() {
            return Ok(());
        }
        crate::value::validate_document_value(document)?;
        fn remove(document: &mut Document, path: &[String]) -> Result<()> {
            let (key, rest) = path.split_first().expect("validated computed path");
            if rest.is_empty() {
                document.remove(key);
                return Ok(());
            }
            match document.get_mut(key) {
                Some(Value::Object(object)) => remove(object, rest),
                None => Ok(()),
                _ => Err(Error::Validation(
                    "computed field requires object parents".into(),
                )),
            }
        }
        for (path, _) in &plan {
            remove(document, path)?;
        }
        let mut budget = crate::links::FetchBudget {
            used: 0,
            limit: 64 * 1024 * 1024,
        };
        serde_json::to_writer(&mut budget, &document)
            .map_err(|_| Error::Limit("computed input/evaluation exceeds 64 MiB".into()))?;
        for (path, expression) in plan {
            if path.len() > 1 && crate::path_value(document, &path[..path.len() - 1])?.is_none() {
                continue;
            }
            let value = self.evaluate_with_budget(
                expression,
                &Parameters::new(),
                Some(document),
                Some(&mut budget),
            )?;
            let mut parent = &mut *document;
            for part in &path[..path.len() - 1] {
                let Some(Value::Object(object)) = parent.get_mut(part) else {
                    return Err(Error::Validation(
                        "computed field requires object parents".into(),
                    ));
                };
                parent = object;
            }
            parent.insert(path.last().expect("validated path").clone(), value);
        }
        Ok(())
    }

    pub(crate) fn validate_computed_fields(
        &self,
        collection: &Collection,
        document: &Document,
    ) -> Result<()> {
        if !collection
            .field_policies
            .iter()
            .any(|policy| policy.options.computed.is_some())
        {
            return Ok(());
        }
        let mut expected = document.clone();
        self.compute_fields(collection, &mut expected)?;
        for policy in collection
            .field_policies
            .iter()
            .filter(|policy| policy.options.computed.is_some())
        {
            let actual = crate::path_value(document, &policy.path)?;
            let expected = crate::path_value(&expected, &policy.path)?;
            let equal = match (actual, expected) {
                (None, None) => true,
                (Some(actual), Some(expected)) => crate::collections::equal(actual, expected),
                _ => false,
            };
            if !equal {
                return Err(Error::Validation(format!(
                    "stored computed field {} does not match its expression",
                    policy.path.join(".")
                )));
            }
        }
        Ok(())
    }
}
