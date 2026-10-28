//! Bounded object, array and record-link projection paths.
use crate::{Error, Result, Value};
use romeql_parser::{Kind, Token};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Part {
    Key(String),
    Index(i64),
    All,
    Fetch(Vec<Vec<String>>),
    Fields(Vec<String>),
    Filter(String),
}

pub(crate) fn decode(encoded: &str) -> Result<Vec<Part>> {
    let parts: Vec<Part> = serde_json::from_str(encoded)?;
    if parts.len() > 64 {
        return Err(Error::Limit("projection path nesting exceeds 64".into()));
    }
    for part in &parts {
        if let Part::Fetch(paths) = part {
            if paths.is_empty()
                || paths.len() > 1024
                || paths.iter().any(|path| path.is_empty() || path.len() > 64)
            {
                return Err(Error::Limit("invalid FETCH path bounds".into()));
            }
        }
        if let Part::Fields(fields) = part {
            if fields.is_empty() || fields.len() > 1024 {
                return Err(Error::Limit(
                    "destructuring requires 1 to 1024 fields".into(),
                ));
            }
        }
        if let Part::Filter(source) = part {
            crate::array_predicate::Predicate::prepare(source)?;
        }
    }
    Ok(parts)
}

/// Evaluate in waves: collect unresolved references across all rows, fetch each
/// wave in batches, then resume. No database reads run inside engine callbacks.
pub(crate) fn project(
    connection: &crate::Connection,
    inputs: &[(Value, Vec<Part>)],
    budget: &mut crate::budget::ResultBudget,
    parameters: &crate::Parameters,
) -> Result<(Vec<Value>, crate::links::FetchMetrics)> {
    use std::collections::BTreeMap;
    struct Walker<'a> {
        output_bytes: crate::links::FetchBudget,
        cache: BTreeMap<String, Value>,
        pending: BTreeMap<String, Value>,
        work: usize,
        parameters: &'a crate::Parameters,
        predicates: BTreeMap<String, std::sync::Arc<crate::array_predicate::Predicate>>,
        predicate_bytes: crate::links::FetchBudget,
    }
    impl Walker<'_> {
        fn step(&mut self, amount: usize) -> Result<()> {
            self.work = self.work.saturating_add(amount);
            if self.work > 1_000_000 {
                return Err(Error::Limit(
                    "projection traversal work exceeds 1000000".into(),
                ));
            }
            Ok(())
        }

        fn reference(&mut self, value: &Value) -> Result<Option<Value>> {
            let Value::Record(record) = value else {
                unreachable!()
            };
            let mut record = record.clone();
            record.table = crate::canonical(&record.table)?;
            let key = serde_json::to_string(&record)?;
            if let Some(document) = self.cache.get(&key) {
                return Ok(Some(document.clone()));
            }
            self.pending.insert(key, value.clone());
            if self.pending.len() + self.cache.len() > crate::links::MAX_FETCH_REFERENCES {
                return Err(Error::Limit("projection references exceed 16384".into()));
            }
            Ok(None)
        }

        fn fetch(&mut self, value: &Value, paths: &[&[String]]) -> Result<Value> {
            self.step(1)?;
            if matches!(value, Value::Record(_)) {
                return match self.reference(value)? {
                    Some(document) => self.fetch(&document, paths),
                    None => Ok(Value::Null),
                };
            }
            if let Value::Array(values) = value {
                return values
                    .iter()
                    .map(|value| self.fetch(value, paths))
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array);
            }
            let Value::Object(document) = value else {
                return self.walk(value, &[]);
            };
            let mut children = BTreeMap::<&str, Vec<&[String]>>::new();
            self.step(paths.len())?;
            for path in paths {
                if let Some((key, rest)) = path.split_first() {
                    children.entry(key).or_default().push(rest);
                }
            }
            if children.is_empty() {
                return self.walk(value, &[]);
            }
            document
                .iter()
                .map(|(key, value)| {
                    let value = match children.get(key.as_str()) {
                        Some(paths) => self.fetch(value, paths)?,
                        None => self.walk(value, &[])?,
                    };
                    Ok((key.clone(), value))
                })
                .collect::<Result<crate::Document>>()
                .map(Value::Object)
        }

        fn walk(&mut self, value: &Value, parts: &[Part]) -> Result<Value> {
            self.step(1)?;
            let Some((part, rest)) = parts.split_first() else {
                self.output_bytes.charge(value)?;
                return Ok(value.clone());
            };
            if let Part::Filter(source) = part {
                let Value::Array(values) = value else {
                    return Ok(Value::Null);
                };
                let predicate = if let Some(predicate) = self.predicates.get(source) {
                    predicate.clone()
                } else {
                    if self.predicates.len() >= 1024 {
                        return Err(Error::Limit(
                            "projection exceeds 1024 array predicates".into(),
                        ));
                    }
                    let predicate =
                        std::sync::Arc::new(crate::array_predicate::Predicate::prepare(source)?);
                    self.predicates.insert(source.clone(), predicate.clone());
                    predicate
                };
                let mut selected = Vec::new();
                for value in values {
                    self.step(predicate.nodes)?;
                    if predicate.matches(value, self.parameters, &mut self.predicate_bytes)? {
                        self.output_bytes.charge(value)?;
                        selected.push(value.clone());
                    }
                }
                let selected = Value::Array(selected);
                return if rest.is_empty() {
                    Ok(selected)
                } else {
                    self.walk(&selected, rest)
                };
            }
            if matches!(value, Value::Record(_)) {
                return match self.reference(value)? {
                    Some(document) => self.walk(&document, parts),
                    None => Ok(Value::Null),
                };
            }
            Ok(match (value, part) {
                (Value::Object(object), Part::Fields(fields)) => {
                    self.step(fields.len())?;
                    let mut selected = crate::Document::new();
                    for field in fields {
                        if let Some(value) = object.get(field) {
                            self.output_bytes.charge(value)?;
                            selected.insert(field.clone(), value.clone());
                        }
                    }
                    let selected = Value::Object(selected);
                    if rest.is_empty() {
                        selected
                    } else {
                        self.walk(&selected, rest)?
                    }
                }
                (Value::Array(values), Part::Fields(_)) => Value::Array(
                    values
                        .iter()
                        .map(|value| self.walk(value, parts))
                        .collect::<Result<_>>()?,
                ),
                (_, Part::Fetch(paths)) => {
                    let paths = paths.iter().map(Vec::as_slice).collect::<Vec<_>>();
                    let value = self.fetch(value, &paths)?;
                    value.validate()?;
                    if rest.is_empty() {
                        value
                    } else {
                        self.walk(&value, rest)?
                    }
                }
                (Value::Object(object), Part::Key(key)) => {
                    self.walk(object.get(key).unwrap_or(&Value::Null), rest)?
                }
                (Value::Array(array), Part::Index(index)) => {
                    let index = if *index >= 0 {
                        usize::try_from(*index).ok()
                    } else {
                        usize::try_from(index.unsigned_abs())
                            .ok()
                            .and_then(|offset| array.len().checked_sub(offset))
                    };
                    self.walk(
                        index.and_then(|i| array.get(i)).unwrap_or(&Value::Null),
                        rest,
                    )?
                }
                (Value::Object(_), Part::All) => self.walk(value, rest)?,
                (Value::Array(array), Part::All) => Value::Array(
                    array
                        .iter()
                        .map(|value| {
                            self.walk(
                                value,
                                if matches!(value, Value::Record(_)) {
                                    parts
                                } else {
                                    rest
                                },
                            )
                        })
                        .collect::<Result<_>>()?,
                ),
                (Value::Array(array), Part::Key(_)) => Value::Array(
                    array
                        .iter()
                        .map(|value| self.walk(value, parts))
                        .collect::<Result<_>>()?,
                ),
                _ => Value::Null,
            })
        }
    }
    let mut walker = Walker {
        output_bytes: crate::links::FetchBudget {
            used: 0,
            limit: crate::links::MAX_FETCH_BYTES,
        },
        cache: BTreeMap::new(),
        pending: BTreeMap::new(),
        work: 0,
        parameters,
        predicates: BTreeMap::new(),
        predicate_bytes: crate::links::FetchBudget {
            used: 0,
            limit: crate::links::MAX_FETCH_BYTES,
        },
    };
    let mut bytes = crate::links::FetchBudget {
        used: 0,
        limit: crate::links::MAX_FETCH_BYTES,
    };
    let mut metrics = crate::links::FetchMetrics::default();
    loop {
        walker.output_bytes.used = 0;
        let output = inputs
            .iter()
            .map(|(value, parts)| walker.walk(value, parts))
            .collect::<Result<Vec<_>>>()?;
        if walker.pending.is_empty() {
            let mut output_bytes = crate::links::FetchBudget {
                used: 0,
                limit: crate::links::MAX_FETCH_BYTES,
            };
            for value in &output {
                output_bytes.charge(value)?;
                budget.value(value)?;
            }
            return Ok((output, metrics));
        }
        let pending = std::mem::take(&mut walker.pending);
        let references = pending.values().cloned().collect::<Vec<_>>();
        let (values, counters) =
            connection.fetch_records_profiled_with_budget(&references, None)?;
        metrics.batches += counters.batches;
        metrics.rows_read += counters.rows_read;
        metrics.vm_steps += counters.vm_steps;
        for (key, value) in pending.into_keys().zip(values) {
            bytes.charge(&value)?;
            walker.cache.insert(key, value);
        }
    }
}

pub(crate) fn expand(sql: &str) -> Result<String> {
    let mut tokens = Vec::new();
    for token in romeql_parser::tokenize(sql)? {
        // The shared SQL tokenizer reads `0.1.` as a number. Within a path,
        // those are two positions and separators. Original SQL bytes remain
        // untouched unless an entire extension path is recognized below.
        if token.kind == Kind::Number
            && token.text.contains('.')
            && token.text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            let extra = token.text.bytes().filter(|b| *b == b'.').count()
                + token
                    .text
                    .split('.')
                    .filter(|part| !part.is_empty())
                    .count();
            if extra > romeql_parser::MAX_TOKENS.saturating_sub(tokens.len()) {
                return Err(Error::Limit("projection token limit exceeded".into()));
            }
            let mut start = token.start;
            for part in token.text.split_inclusive('.') {
                let number = part.trim_end_matches('.');
                if !number.is_empty() {
                    tokens.push(Token {
                        kind: Kind::Number,
                        text: number.into(),
                        start,
                        end: start + number.len(),
                    });
                    start += number.len();
                }
                if part.ends_with('.') {
                    tokens.push(Token {
                        kind: Kind::Symbol,
                        text: ".".into(),
                        start,
                        end: start + 1,
                    });
                    start += 1;
                }
            }
        } else {
            if tokens.len() == romeql_parser::MAX_TOKENS {
                return Err(Error::Limit("projection token limit exceeded".into()));
            }
            tokens.push(token);
        }
    }
    let name = |t: &Token| matches!(t.kind, Kind::Word | Kind::Identifier);
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;
    while i < tokens.len() {
        if !name(&tokens[i]) || !path_root(&tokens, i) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        let mut base_end = i;
        let mut special = false;
        let mut parts = Vec::new();
        let mut depth = 0;
        loop {
            // Adjacent bracket positions are RomeQL paths; spaced bracket names
            // retain SQL alias/identifier parsing.
            if let Some(token) = tokens
                .get(end + 1)
                .filter(|t| t.start == tokens[end].end && sql.as_bytes()[t.start] == b'[')
            {
                let literal = token.text.trim();
                if literal
                    .get(..5)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("WHERE"))
                    && literal
                        .as_bytes()
                        .get(5)
                        .is_some_and(u8::is_ascii_whitespace)
                {
                    let predicate = literal[5..].trim().to_owned();
                    crate::array_predicate::Predicate::prepare(&predicate)?;
                    special = true;
                    parts.push(Part::Filter(predicate));
                    end += 1;
                    depth += 1;
                    if depth > 64 {
                        return Err(Error::Limit("projection path nesting exceeds 64".into()));
                    }
                    continue;
                }
                let index = if literal == "$" {
                    Some(-1)
                } else {
                    literal.parse::<i64>().ok()
                };
                if let Some(index) = index {
                    special = true;
                    parts.push(Part::Index(index));
                    end += 1;
                    depth += 1;
                    if depth > 64 {
                        return Err(Error::Limit("projection path nesting exceeds 64".into()));
                    }
                    continue;
                }
            }
            if tokens.get(end + 1).is_none_or(|t| t.text != ".") {
                break;
            }
            let Some(next) = tokens.get(end + 2) else {
                break;
            };
            let mut next_end = end + 2;
            let part = if name(next) {
                Part::Key(next.text.clone())
            } else if next.kind == Kind::Symbol && next.text == "{" {
                let mut fields = Vec::new();
                let mut seen = std::collections::BTreeSet::new();
                next_end += 1;
                loop {
                    let Some(field) = tokens.get(next_end).filter(|field| name(field)) else {
                        return Err(Error::Validation(
                            "destructuring requires named fields".into(),
                        ));
                    };
                    if !seen.insert(field.text.clone()) {
                        return Err(Error::Validation("duplicate destructured field".into()));
                    }
                    fields.push(field.text.clone());
                    if fields.len() > 1024 {
                        return Err(Error::Limit("destructuring exceeds 1024 fields".into()));
                    }
                    next_end += 1;
                    match tokens
                        .get(next_end)
                        .filter(|token| token.kind == Kind::Symbol)
                        .map(|token| token.text.as_str())
                    {
                        Some("}") => break,
                        Some(",") => next_end += 1,
                        _ => {
                            return Err(Error::Validation(
                                "destructuring expects comma or closing brace".into(),
                            ))
                        }
                    }
                }
                special = true;
                Part::Fields(fields)
            } else if next.text == "*" {
                special = true;
                Part::All
            } else {
                let negative = next.text == "-";
                let number = if negative {
                    let Some(t) = tokens.get(end + 3) else { break };
                    next_end += 1;
                    t
                } else {
                    next
                };
                if number.kind != Kind::Number || !number.text.bytes().all(|b| b.is_ascii_digit()) {
                    break;
                }
                let literal = if negative {
                    format!("-{}", number.text)
                } else {
                    number.text.clone()
                };
                let index = literal.parse::<i64>().map_err(|_| {
                    Error::Validation("array position exceeds signed 64-bit range".into())
                })?;
                special = true;
                Part::Index(index)
            };
            end = next_end;
            if special {
                parts.push(part);
            } else {
                base_end = end;
            }
            depth += 1;
            if depth > 64 {
                return Err(Error::Limit("projection path nesting exceeds 64".into()));
            }
        }
        let single_star = base_end == start && matches!(parts.as_slice(), [Part::All]);
        let aliased = tokens
            .get(end + 1)
            .is_some_and(|t| t.kind == Kind::Word && t.text.eq_ignore_ascii_case("AS"));
        if special && array_contains_argument(&tokens, start, end) {
            if let Some(path) = array_path(&tokens[base_end].text, &parts)? {
                let base = &sql[tokens[start].start..tokens[base_end].end];
                out.push_str(&sql[copied..tokens[start].start]);
                out.push_str(&format!("__romedb_array_path({base},'{path}')"));
                copied = tokens[end].end;
                i = end + 1;
                continue;
            }
        }
        if special && (!single_star || aliased) {
            let base = &sql[tokens[start].start..tokens[base_end].end];
            let encoded = serde_json::to_string(&parts)?.replace('\'', "''");
            out.push_str(&sql[copied..tokens[start].start]);
            out.push_str(&format!("__romedb_h_doc_project({base},'{encoded}')"));
            copied = tokens[end].end;
        }
        i = end + 1;
    }
    out.push_str(&sql[copied..]);
    Ok(out)
}

fn array_contains_argument(tokens: &[Token], start: usize, end: usize) -> bool {
    let first_argument = tokens.get(end + 1).is_some_and(|token| token.text == ",")
        && start
            .checked_sub(1)
            .is_some_and(|open| tokens[open].text == "(");
    if !first_argument {
        return false;
    }
    let helper = start
        .checked_sub(2)
        .map(|i| &tokens[i])
        .is_some_and(|token| token.kind == Kind::Word && token.text == "__romedb_h_array_contains");
    let namespaced = start.checked_sub(5).is_some_and(|first| {
        let before = &tokens[first..start - 1];
        before.windows(2).all(|pair| pair[0].end == pair[1].start)
            && before[0].kind == Kind::Word
            && before[0].text.eq_ignore_ascii_case("array")
            && before[1].text == ":"
            && before[2].text == ":"
            && before[3].kind == Kind::Word
            && before[3].text.eq_ignore_ascii_case("contains")
    });
    helper || namespaced
}

fn array_path(field: &str, parts: &[Part]) -> Result<Option<String>> {
    if !matches!(parts.last(), Some(Part::Key(_)))
        || !parts.iter().any(|part| matches!(part, Part::All))
    {
        return Ok(None);
    }
    let mut path = vec![field.to_owned()];
    for part in parts {
        match part {
            Part::All => path.push(crate::array_index::ARRAY_STEP.to_owned()),
            Part::Key(key) if key == crate::array_index::ARRAY_STEP => {
                return Err(Error::Validation(
                    "a field named * cannot be used in an array path".into(),
                ))
            }
            Part::Key(key) => path.push(key.clone()),
            _ => return Ok(None),
        }
    }
    Ok(Some(serde_json::to_string(&path)?.replace('\'', "''")))
}

fn path_root(tokens: &[Token], index: usize) -> bool {
    let token = &tokens[index];
    if token.kind != Kind::Word {
        return true;
    }
    // A bracketed SQL identifier needs no preceding whitespace: SELECT[0],
    // FROM[0] and DISTINCT[0] are not array operations on those keywords.
    use turso_parser::{lexer::Lexer, token::TokenType};
    let Some(Ok(native)) = Lexer::new(token.text.as_bytes()).next() else {
        return false;
    };
    if native.token_type.fallback_id_if_ok() != TokenType::TK_ID {
        return false;
    }
    if !turso_parser::lexer::is_quotable_keyword(token.text.as_bytes()) {
        return true;
    }
    // Contextual keywords can still name fields, e.g. SELECT rows[0]. Only
    // consider them at expression starts, so ORDER BY[0], LIMIT 1 OFFSET[0]
    // and WITH[0] retain their SQL clause/identifier meanings.
    index.checked_sub(1).is_some_and(|previous| {
        let previous = &tokens[previous];
        match previous.kind {
            Kind::Symbol => matches!(
                previous.text.as_str(),
                "(" | "," | "+" | "-" | "*" | "/" | "%" | "=" | "<" | ">" | "!" | "|" | "&" | "~"
            ),
            Kind::Word => matches!(
                previous.text.to_ascii_uppercase().as_str(),
                "SELECT"
                    | "DISTINCT"
                    | "ALL"
                    | "WHERE"
                    | "HAVING"
                    | "ON"
                    | "AND"
                    | "OR"
                    | "NOT"
                    | "WHEN"
                    | "THEN"
                    | "ELSE"
                    | "BY"
                    | "LIMIT"
                    | "OFFSET"
            ),
            _ => false,
        }
    })
}

/// Star resolution happens after parsing, so detect even spaced/commented
/// field stars before starting the snapshot shared by source and target reads.
pub(crate) fn has_wildcard(sql: &str) -> Result<bool> {
    Ok(romeql_parser::tokenize(sql)?
        .windows(2)
        .any(|tokens| tokens[0].text == "." && tokens[1].text == "*"))
}
