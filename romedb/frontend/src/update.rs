//! RomeQL field-path assignments are normalized before the stock SQL parser.
use crate::{quote, Document, Error, Result, Value};
use romeql_parser::{Kind, Token};

pub(crate) struct UpdateSyntax {
    pub sql: String,
    pub paths: Vec<Vec<String>>,
    pub unset: bool,
}
fn word(t: &Token, s: &str) -> bool {
    t.kind == Kind::Word && t.text.eq_ignore_ascii_case(s)
}
fn path(tokens: &[Token]) -> Option<Vec<String>> {
    if tokens.is_empty() || tokens.len() % 2 == 0 {
        return None;
    }
    let mut path = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if i % 2 == 0 {
            if !matches!(t.kind, Kind::Word | Kind::Identifier) {
                return None;
            }
            path.push(t.text.clone());
        } else if t.kind != Kind::Symbol || t.text != "." {
            return None;
        }
    }
    Some(path)
}
pub(crate) fn normalize(sql: &str) -> Result<Option<UpdateSyntax>> {
    let tokens = romeql_parser::tokenize(sql)?;
    if tokens.first().is_none_or(|t| !word(t, "UPDATE")) {
        return Ok(None);
    }
    let Some(set) = tokens
        .iter()
        .position(|t| word(t, "SET") || word(t, "UNSET"))
    else {
        return Ok(None);
    };
    let unset = word(&tokens[set], "UNSET");
    let mut paths = Vec::new();
    let mut assignments = Vec::new();
    let mut start = set + 1;
    let mut depth = 0;
    let mut end = tokens.len();
    for i in set + 1..=tokens.len() {
        let t = tokens.get(i);
        let boundary = depth == 0
            && t.is_none_or(|t| {
                t.text == "," || t.text == ";" || word(t, "WHERE") || word(t, "RETURNING")
            });
        if boundary {
            let slice = &tokens[start..i];
            let (lhs, rhs) = if unset {
                (slice, "NULL")
            } else {
                let Some(eq) = slice
                    .iter()
                    .position(|t| t.kind == Kind::Symbol && t.text == "=")
                else {
                    return Ok(None);
                };
                let Some(first) = slice.get(eq + 1) else {
                    return Ok(None);
                };
                (
                    &slice[..eq],
                    &sql[first.start..slice.last().expect("nonempty assignment").end],
                )
            };
            let Some(p) = path(lhs) else {
                return Ok(None);
            };
            assignments.push(format!("{} = {}", quote(&p.join(".")), rhs));
            paths.push(p);
            if t.is_some_and(|t| t.text == ",") {
                start = i + 1;
            } else {
                end = i;
                break;
            }
        }
        if let Some(t) = t {
            if t.kind == Kind::Symbol {
                if t.text == "(" {
                    depth += 1;
                }
                if t.text == ")" {
                    depth -= 1;
                }
            }
        }
    }
    let target = &sql[tokens[0].end..tokens[set].start];
    let direct = match romeql_parser::parse(&format!("SELECT {}", target.trim())) {
        Ok(romeql_parser::Statement::SelectRecord(r)) => Some(r),
        _ => None,
    };
    let suffix_start = tokens.get(end).map_or(sql.len(), |t| t.start);
    let mut suffix = sql[suffix_start..].to_owned();
    let target = if let Some(record) = direct {
        if tokens.get(end).is_some_and(|t| word(t, "WHERE")) {
            return Err(Error::Unsupported(
                "direct UPDATE targets do not accept an additional WHERE".into(),
            ));
        }
        let key = match record.key {
            crate::Key::Integer(i) => i.to_string(),
            crate::Key::String(s) => format!("'{}'", s.replace('\'', "''")),
        };
        suffix = format!(
            "WHERE id = type::record('{}', {}) {}",
            record.table.replace('\'', "''"),
            key,
            suffix
        );
        quote(&record.table)
    } else {
        target.trim().into()
    };
    Ok(Some(UpdateSyntax {
        sql: format!(
            "UPDATE {} SET {} {}",
            target,
            assignments.join(", "),
            suffix
        ),
        paths,
        unset,
    }))
}
pub(crate) fn validate_targets(paths: &[Vec<String>]) -> Result<()> {
    for (i, path) in paths.iter().enumerate() {
        crate::validate_path(path)?;
        if path[0] == "id" {
            return Err(Error::Validation("id is immutable".into()));
        }
        if paths[..i]
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            return Err(Error::Validation(
                "duplicate or overlapping assignment targets".into(),
            ));
        }
    }
    Ok(())
}
pub(crate) fn apply(doc: &mut Document, path: &[String], value: Option<Value>) -> Result<()> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| Error::Validation("empty assignment path".into()))?;
    let mut fields = doc;
    for part in parents {
        if value.is_none() && !fields.contains_key(part) {
            return Ok(());
        }
        let parent = fields
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Document::new()));
        match parent {
            Value::Object(object) => fields = object,
            _ if value.is_none() => return Ok(()),
            _ => {
                return Err(Error::Validation(format!(
                    "non-object assignment parent {part}"
                )))
            }
        }
    }
    if let Some(value) = value {
        fields.insert(last.clone(), value);
    } else {
        fields.remove(last);
    }
    Ok(())
}
