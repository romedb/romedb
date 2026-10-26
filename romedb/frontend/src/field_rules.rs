use crate::{Collection, Document, Error, Field, FieldType, Result, Value};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FieldOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computed: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flexible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element_type: Option<FieldType>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub element_nullable: bool,
}

pub(crate) fn is_false(value: &bool) -> bool {
    !value
}

impl FieldOptions {
    pub(crate) fn is_empty(&self) -> bool {
        self.default.is_none()
            && self.computed.is_none()
            && !self.readonly
            && !self.flexible
            && self.element_type.is_none()
            && !self.element_nullable
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
    pub(crate) path: Vec<String>,
    pub(crate) options: FieldOptions,
}

pub(crate) fn matches_kind(kind: &FieldType, value: &Value) -> Result<bool> {
    Ok(match (kind, value) {
        (FieldType::String, Value::String(_))
        | (FieldType::Integer, Value::Integer(_))
        | (FieldType::Number, Value::Integer(_) | Value::Number(_))
        | (FieldType::Boolean, Value::Boolean(_))
        | (FieldType::Object, Value::Object(_))
        | (FieldType::Array, Value::Array(_)) => true,
        (FieldType::Record(target), Value::Record(record)) => {
            target.eq_ignore_ascii_case(&record.table)
        }
        (FieldType::Vector(dimensions), Value::Vector(bytes)) => {
            crate::vectors::dimensions(bytes)? == *dimensions
        }
        _ => false,
    })
}

pub(crate) fn validate_options(field: &Field, options: &FieldOptions) -> Result<()> {
    crate::computed::validate_options(options)?;
    if !options.is_empty() && field.path.len() > 64 {
        return Err(Error::Limit("field rules exceed 64 path steps".into()));
    }
    if options.flexible && !matches!(field.kind, FieldType::Object | FieldType::Array) {
        return Err(Error::Validation(
            "FLEXIBLE requires an object or array field".into(),
        ));
    }
    if options.element_type.is_some() && !matches!(field.kind, FieldType::Array) {
        return Err(Error::Validation(
            "element type requires an array field".into(),
        ));
    }
    if options.element_nullable && options.element_type.is_none() {
        return Err(Error::Validation(
            "element nullability requires an element type".into(),
        ));
    }
    if let Some(kind) = &options.element_type {
        match kind {
            FieldType::Vector(dimensions) => crate::vectors::validate_dimension(*dimensions)?,
            FieldType::Record(target) => {
                crate::canonical(target)?;
            }
            _ => {}
        }
    }
    if let Some(value) = &options.default {
        value.encode_with_limit(Some(64 * 1024))?;
        let mut nested = value.clone();
        for part in field.path.iter().rev() {
            nested = Value::Object(Document::from([(part.clone(), nested)]));
        }
        nested.validate()?;
        if !(matches!(value, Value::Null) && field.nullable || matches_kind(&field.kind, value)?) {
            return Err(Error::Validation(
                "default does not match field type/nullability".into(),
            ));
        }
        validate_elements(value, options, &mut 0)?;
    }
    Ok(())
}

pub(crate) fn validate_catalog(collection: &Collection) -> Result<()> {
    crate::schema_rules::validate_definition(collection)?;
    if collection.field_policies.len() > 1024 {
        return Err(Error::Limit("collection exceeds 1024 field rules".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for policy in &collection.field_policies {
        let field = collection
            .fields
            .iter()
            .find(|field| field.path == policy.path)
            .ok_or_else(|| Error::Storage("field rule has no field definition".into()))?;
        if !seen.insert(&policy.path) || policy.options.is_empty() {
            return Err(Error::Storage("invalid or duplicate field rule".into()));
        }
        validate_options(field, &policy.options)?;
    }
    crate::computed::validate_catalog(collection)
}

pub(crate) fn defaults(collection: &Collection, document: &mut Document) -> Result<()> {
    let mut policies = collection.field_policies.iter().collect::<Vec<_>>();
    policies.sort_by(|a, b| a.path.len().cmp(&b.path.len()).then(a.path.cmp(&b.path)));
    fn apply(document: &mut Document, path: &[String], value: &Value) {
        let (key, rest) = path.split_first().expect("validated field path");
        if rest.is_empty() {
            if !document.contains_key(key) {
                document.insert(key.clone(), value.clone());
            }
        } else if let Some(Value::Object(object)) = document.get_mut(key) {
            apply(object, rest, value);
        }
    }
    for policy in policies {
        if let Some(value) = &policy.options.default {
            apply(document, &policy.path, value);
        }
    }
    Ok(())
}

pub(crate) fn readonly(collection: &Collection, before: &Document, after: &Document) -> Result<()> {
    for policy in collection
        .field_policies
        .iter()
        .filter(|policy| policy.options.readonly)
    {
        let before = crate::path_value(before, &policy.path)?;
        let after = crate::path_value(after, &policy.path)?;
        let equal = match (before, after) {
            (None, None) => true,
            (Some(before), Some(after)) => crate::collections::equal(before, after),
            _ => false,
        };
        if !equal {
            return Err(Error::Validation(format!(
                "read-only field {} cannot change or be removed",
                policy.path.join(".")
            )));
        }
    }
    Ok(())
}

pub(crate) fn constant(expression: &romeql_parser::Expr) -> Result<()> {
    use romeql_parser::Expr;
    match expression {
        Expr::Field(_) | Expr::Call(_,_)=>return Err(Error::Unsupported("DEFAULT accepts constant expressions and typed parameters, without fields or function calls".into())),
        Expr::Unary(_,value)=>constant(value)?,
        Expr::Binary(left,_,right)=>{constant(left)?;constant(right)?;}
        Expr::Object(fields)=>{for value in fields.values(){constant(value)?;}}
        Expr::Array(values)=>{for value in values{constant(value)?;}}
        Expr::Case{base,branches,fallback}=>{
            for value in base.iter().chain(fallback){constant(value)?;}
            for (condition,value) in branches {constant(condition)?;constant(value)?;}
        }
        _=>{}
    }
    Ok(())
}

pub(crate) fn type_name(kind: &FieldType) -> String {
    match kind {
        FieldType::String => "string".into(),
        FieldType::Integer => "integer".into(),
        FieldType::Number => "number".into(),
        FieldType::Boolean => "boolean".into(),
        FieldType::Object => "object".into(),
        FieldType::Array => "array".into(),
        FieldType::Record(target) => format!("record<{target}>"),
        FieldType::Vector(dimensions) => format!("vector<{dimensions}>"),
    }
}

pub(crate) fn element_type(target: Option<&str>) -> Result<(Option<FieldType>, bool)> {
    let Some(target) = target else {
        return Ok((None, false));
    };
    let nullable = target.ends_with('?');
    let target = target
        .strip_suffix('?')
        .unwrap_or(target)
        .to_ascii_lowercase();
    let kind = match target.as_str() {
        "any" => return Ok((None, false)),
        "string" => FieldType::String,
        "integer" => FieldType::Integer,
        "number" => FieldType::Number,
        "boolean" => FieldType::Boolean,
        "object" => FieldType::Object,
        "array" => FieldType::Array,
        _ if target.starts_with("record<") && target.ends_with('>') => {
            FieldType::Record(crate::canonical(&target[7..target.len() - 1])?)
        }
        _ if target.starts_with("vector<") && target.ends_with('>') => FieldType::Vector(
            target[7..target.len() - 1]
                .parse()
                .map_err(|_| Error::Validation("invalid element vector dimension".into()))?,
        ),
        _ => return Err(Error::Unsupported("unsupported array element type".into())),
    };
    Ok((Some(kind), nullable))
}

fn validate_elements(value: &Value, options: &FieldOptions, work: &mut usize) -> Result<()> {
    let (Some(kind), Value::Array(values)) = (&options.element_type, value) else {
        return Ok(());
    };
    *work = work.saturating_add(values.len());
    if *work > 1_000_000 {
        return Err(Error::Limit(
            "typed array validation exceeds 1000000 elements".into(),
        ));
    }
    for (index, value) in values.iter().enumerate() {
        if !(matches!(value, Value::Null) && options.element_nullable || matches_kind(kind, value)?)
        {
            return Err(Error::Validation(format!(
                "array element {index} failed {} validation",
                type_name(kind)
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_arrays(collection: &Collection, document: &Document) -> Result<()> {
    let mut work = 0;
    for policy in &collection.field_policies {
        if policy.options.element_type.is_some() {
            if let Some(value) = crate::path_value(document, &policy.path)? {
                validate_elements(value, &policy.options, &mut work)?;
            }
        }
    }
    Ok(())
}
