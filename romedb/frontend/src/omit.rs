use crate::{Document, Error, Result, Value};
use romeql_parser::{Kind, Token};

pub(crate) fn expand(sql: &str) -> Result<String> {
    let tokens = romeql_parser::tokenize(sql)?;
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i + 2 < tokens.len() {
        if !word(&tokens[i], "SELECT")
            || tokens[i + 1].kind != Kind::Symbol
            || tokens[i + 1].text != "*"
            || !word(&tokens[i + 2], "OMIT")
        {
            i += 1;
            continue;
        }
        let start = tokens[i + 1].start;
        i += 3;
        let mut paths = Vec::new();
        loop {
            let mut path = Vec::new();
            loop {
                let token = tokens.get(i).ok_or_else(invalid)?;
                if !matches!(token.kind, Kind::Word | Kind::Identifier) {
                    return Err(invalid());
                }
                path.push(token.text.clone());
                if path.len() > 64 {
                    return Err(Error::Limit("OMIT path nesting exceeds 64".into()));
                }
                i += 1;
                if tokens.get(i).is_none_or(|t| t.text != ".") {
                    break;
                }
                i += 1;
            }
            paths.push(path);
            if paths.len() > 1024 {
                return Err(Error::Limit("OMIT paths exceed 1024".into()));
            }
            if tokens.get(i).is_none_or(|t| t.text != ",") {
                break;
            }
            i += 1;
        }
        let from = tokens
            .get(i)
            .filter(|t| word(t, "FROM"))
            .ok_or_else(invalid)?;
        let paths = serde_json::to_string(&paths)?.replace('\'', "''");
        out.push_str(&sql[copied..start]);
        out.push_str(&format!("__romedb_h_doc_omit_row('{paths}') AS document "));
        copied = from.start;
    }
    out.push_str(&sql[copied..]);
    Ok(out)
}

fn word(token: &Token, expected: &str) -> bool {
    token.kind == Kind::Word && token.text.eq_ignore_ascii_case(expected)
}

fn invalid() -> Error {
    Error::Validation("OMIT requires object field paths followed by FROM".into())
}

pub(crate) fn apply(document: &Document, encoded_paths: &str) -> Result<Value> {
    let paths: Vec<Vec<String>> = serde_json::from_str(encoded_paths)?;
    if paths.len() > 1024 || paths.iter().any(|path| path.is_empty() || path.len() > 64) {
        return Err(Error::Limit("invalid OMIT path bounds".into()));
    }
    let mut document = document.clone();
    for path in &paths {
        remove(&mut document, path);
    }
    Ok(Value::Object(document))
}

fn remove(document: &mut Document, path: &[String]) {
    let Some((key, rest)) = path.split_first() else {
        return;
    };
    if rest.is_empty() {
        document.remove(key);
    } else if let Some(Value::Object(child)) = document.get_mut(key) {
        remove(child, rest);
    }
}
