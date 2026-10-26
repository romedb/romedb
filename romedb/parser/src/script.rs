//! Lexical script boundaries shared by the Rust batch API and CLI.
use crate::{tokenize, Error, Kind, Result};
#[derive(Debug, PartialEq)]
pub struct ScriptStatement<'a> {
    pub sql: &'a str,
    pub offset: usize,
}
pub fn split_script(input: &str) -> Result<Vec<ScriptStatement<'_>>> {
    split_state(input).map(|(statements, _)| statements)
}
/// Whether input contains only terminated statements (or whitespace/comments).
/// Open quotes, comments, delimiters and trigger bodies request more input.
pub fn script_complete(input: &str) -> Result<bool> {
    match split_state(input) {
        Ok((_, complete)) => Ok(complete),
        Err(error)
            if matches!(
                error.message.as_str(),
                "unterminated quote" | "unterminated comment" | "unclosed script delimiter"
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
fn split_state(input: &str) -> Result<(Vec<ScriptStatement<'_>>, bool)> {
    let tokens = tokenize(input)?;
    let mut statements = Vec::new();
    let mut start = 0;
    let mut depth: i32 = 0;
    for (i, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Symbol {
            continue;
        }
        match token.text.as_str() {
            "(" | "{" | "[" => depth += 1,
            ")" | "}" | "]" => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return Err(Error {
                offset: token.start,
                message: "unmatched script delimiter".into(),
            });
        }
        if token.text != ";" || depth != 0 {
            continue;
        }
        if i == start {
            start = i + 1;
            continue;
        }
        let word = |n: usize, text: &str| {
            tokens
                .get(start + n)
                .is_some_and(|t| t.kind == Kind::Word && t.text.eq_ignore_ascii_case(text))
        };
        let trigger = word(0, "CREATE")
            && (word(1, "TRIGGER")
                || ((word(1, "TEMP") || word(1, "TEMPORARY")) && word(2, "TRIGGER")));
        // SQLite trigger bodies end with '; END ;'. A CASE's END does not
        // have a preceding statement terminator, and quoted END is data.
        if trigger
            && !(i >= start + 2
                && tokens[i - 1].kind == Kind::Word
                && tokens[i - 1].text.eq_ignore_ascii_case("END")
                && tokens[i - 2].kind == Kind::Symbol
                && tokens[i - 2].text == ";")
        {
            continue;
        }
        let offset = tokens[start].start;
        statements.push(ScriptStatement {
            sql: &input[offset..token.end],
            offset,
        });
        start = i + 1;
    }
    if depth != 0 {
        return Err(Error {
            offset: input.len(),
            message: "unclosed script delimiter".into(),
        });
    }
    if start < tokens.len() {
        let offset = tokens[start].start;
        statements.push(ScriptStatement {
            sql: &input[offset..tokens.last().expect("nonempty").end],
            offset,
        });
    }
    Ok((statements, start == tokens.len()))
}
