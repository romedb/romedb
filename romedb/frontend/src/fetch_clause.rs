use crate::{Error, Result};
use romeql_parser::{Kind, Token};
use turso_parser::ast::{Cmd, OneSelect, ResultColumn, Stmt};

pub(crate) fn expand(sql: &str) -> Result<String> {
    let tokens = romeql_parser::tokenize(sql)?;
    let mut depth = 0usize;
    for (position, token) in tokens.iter().enumerate() {
        if token.kind == Kind::Symbol {
            match token.text.as_str() {
                "(" | "{" | "[" => depth += 1,
                ")" | "}" | "]" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if depth != 0 || token.kind != Kind::Word || !token.text.eq_ignore_ascii_case("FETCH") {
            continue;
        }
        let Some(paths) = paths(&tokens[position + 1..])? else {
            continue;
        };
        let Ok(mut command) = crate::select::parsed(&sql[..token.start]) else {
            continue;
        };
        let statement = match &mut command {
            Cmd::Stmt(statement)
            | Cmd::Explain(statement)
            | Cmd::ExplainQueryPlan {
                stmt: statement, ..
            } => statement,
        };
        let Stmt::Select(select) = statement else {
            continue;
        };
        if !select.body.compounds.is_empty() {
            return Err(Error::Unsupported("FETCH on compound queries".into()));
        }
        let OneSelect::Select {
            columns,
            from: Some(_),
            ..
        } = &mut select.body.select
        else {
            return Err(Error::Unsupported(
                "FETCH requires SELECT * from one collection".into(),
            ));
        };
        if !matches!(columns.as_slice(), [ResultColumn::Star]) {
            return Err(Error::Unsupported(
                "FETCH requires one whole-document result".into(),
            ));
        }
        let encoded = serde_json::json!([{"Fetch":paths}])
            .to_string()
            .replace('\'', "''");
        let Cmd::Stmt(Stmt::Select(projection)) = crate::select::parsed(&format!(
            "SELECT __romedb_h_doc_project(__romedb_h_doc_fetch_source(),'{encoded}') AS document"
        ))?
        else {
            unreachable!()
        };
        let OneSelect::Select {
            columns: replacement,
            ..
        } = projection.body.select
        else {
            unreachable!()
        };
        *columns = replacement;
        return Ok(command.to_string());
    }
    Ok(sql.to_owned())
}

fn paths(mut tokens: &[Token]) -> Result<Option<Vec<Vec<String>>>> {
    if tokens
        .last()
        .is_some_and(|token| token.kind == Kind::Symbol && token.text == ";")
    {
        tokens = &tokens[..tokens.len() - 1];
    }
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut paths = Vec::new();
    let mut position = 0;
    loop {
        let mut path = Vec::new();
        loop {
            let Some(token) = tokens.get(position) else {
                return Ok(None);
            };
            if !matches!(token.kind, Kind::Word | Kind::Identifier) {
                return Ok(None);
            }
            path.push(token.text.clone());
            if path.len() > 64 {
                return Err(Error::Limit("FETCH path exceeds 64 steps".into()));
            }
            position += 1;
            if tokens
                .get(position)
                .is_none_or(|token| token.kind != Kind::Symbol || token.text != ".")
            {
                break;
            }
            position += 1;
        }
        paths.push(path);
        if paths.len() > 1024 {
            return Err(Error::Limit("FETCH exceeds 1024 paths".into()));
        }
        if position == tokens.len() {
            break;
        }
        if tokens[position].kind != Kind::Symbol || tokens[position].text != "," {
            return Ok(None);
        }
        position += 1;
    }
    paths.sort();
    paths.dedup();
    Ok(Some(paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_keeps_fetch_names_and_whole_document_rules() {
        for sql in [
            "SELECT * FROM fetch",
            "SELECT * FROM docs fetch",
            "SELECT * FROM docs fetch WHERE fetch.n=1",
            "SELECT * FROM docs AS fetch ORDER BY fetch.id LIMIT 1",
            "SELECT fetch FROM docs",
            "SELECT * FROM docs WHERE fetch=1",
            "SELECT * FROM docs ORDER BY fetch",
        ] {
            assert_eq!(expand(sql).unwrap(), sql, "{sql}");
        }
        let sql=expand("SELECT * FROM docs WHERE n=$n ORDER BY id LIMIT 3 FETCH author, metadata.editor, author;").unwrap();
        assert!(sql.contains("__romedb_h_doc_fetch_source"));
        assert!(sql.contains("metadata"));
        for sql in [
            "SELECT id FROM docs FETCH author",
            "SELECT * FROM docs UNION ALL SELECT * FROM docs FETCH author",
        ] {
            assert!(expand(sql).is_err(), "{sql}");
        }
    }
}
