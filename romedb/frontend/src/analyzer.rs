use crate::{Document, Error, Result, Value};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct FullTextOptions {
    pub tokenizer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gram: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gram: Option<usize>,
}

impl Default for FullTextOptions {
    fn default() -> Self {
        Self {
            tokenizer: "default".into(),
            min_gram: None,
            max_gram: None,
        }
    }
}

impl FullTextOptions {
    pub(crate) fn normalized(&self) -> Result<Self> {
        let mut options = self.clone();
        options.tokenizer.make_ascii_lowercase();
        if !turso_core::index_method::fts::SUPPORTED_TOKENIZERS
            .contains(&options.tokenizer.as_str())
        {
            return Err(Error::Validation("unsupported full-text tokenizer".into()));
        }
        if options.tokenizer == "ngram" {
            let min = options.min_gram.unwrap_or(2);
            let max = options.max_gram.unwrap_or(3);
            if min == 0 || min > max || max > 64 {
                return Err(Error::Validation(
                    "ngram sizes require 1 <= min_gram <= max_gram <= 64".into(),
                ));
            }
            options.min_gram = Some(min);
            options.max_gram = Some(max);
        } else if options.min_gram.is_some() || options.max_gram.is_some() {
            return Err(Error::Validation(
                "ngram sizes require the ngram tokenizer".into(),
            ));
        }
        Ok(options)
    }
    pub(crate) fn identity(&self) -> String {
        format!("tantivy-{}-0.26", self.tokenizer)
    }
}

pub(crate) fn analyze(options: &FullTextOptions, input: &str) -> Result<Value> {
    let options = options.normalized()?;
    if input.len() > 1024 * 1024 {
        return Err(Error::Limit("analyzer input exceeds 1 MiB".into()));
    }
    let mut output = Vec::new();
    let mut bytes = crate::links::FetchBudget {
        used: 0,
        limit: 8 * 1024 * 1024,
    };
    turso_core::index_method::fts::analyze_text::<Error>(
        &options.tokenizer,
        (options.min_gram.unwrap_or(2), options.max_gram.unwrap_or(3)),
        input,
        |token| {
            if output.len() >= 16_384 {
                return Err(Error::Limit("analyzer output exceeds 16384 tokens".into()));
            }
            let value = Value::Object(Document::from([
                ("text".into(), Value::String(token.text.into())),
                (
                    "offset_from".into(),
                    Value::Integer(token.offset_from as i64),
                ),
                ("offset_to".into(), Value::Integer(token.offset_to as i64)),
                ("position".into(), Value::Integer(token.position as i64)),
                (
                    "position_length".into(),
                    Value::Integer(token.position_length as i64),
                ),
            ]));
            bytes.charge(&value)?;
            output.push(value);
            Ok(())
        },
    )?;
    Ok(Value::Array(output))
}

pub(crate) fn call(arguments: &[Value]) -> Result<Value> {
    match arguments {
        [Value::String(config), Value::String(_index), value] => {
            let options: FullTextOptions = serde_json::from_str(config).map_err(|error| {
                Error::Storage(format!("invalid analyzer configuration: {error}"))
            })?;
            match value {
                Value::String(input) => analyze(&options, input),
                Value::Null => Ok(Value::Null),
                _ => Err(Error::Validation(
                    "search::analyze expects text or null".into(),
                )),
            }
        }
        _ => Err(Error::Validation(
            "search::analyze expects an index name and text".into(),
        )),
    }
}

pub(crate) fn has_calls(sql: &str) -> Result<bool> {
    let tokens = romeql_parser::tokenize(sql)?;
    Ok(tokens.windows(5).any(|parts| {
        parts[0].kind == romeql_parser::Kind::Word
            && parts[0].text.eq_ignore_ascii_case("search")
            && parts[1].text == ":"
            && parts[2].text == ":"
            && parts[3].text.eq_ignore_ascii_case("analyze")
            && parts[4].text == "("
    }))
}

impl crate::Connection {
    pub(crate) fn analyzer_options(&self, name: &str) -> Result<FullTextOptions> {
        let name = crate::canonical(name)?;
        for collection in self.collections()? {
            if let Some(index) = collection.indexes.iter().find(|index| index.name == name) {
                index.require_current_text_storage()?;
                return index
                    .fulltext
                    .as_ref()
                    .ok_or_else(|| {
                        Error::Validation("search::analyze requires a full-text index".into())
                    })?
                    .options();
            }
        }
        Err(Error::NotFound(name))
    }
    pub fn analyze_text(&self, index: &str, input: &str) -> Result<Value> {
        self.atomic(|| analyze(&self.analyzer_options(index)?, input))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn persisted_options_require_matching_version_canonical_metadata_and_owned_ddl() {
        let db = crate::Database::open(":memory:").unwrap();
        let c = db.connect().unwrap();
        c.execute("CREATE TABLE docs", &crate::Parameters::new())
            .unwrap();
        c.execute("CREATE SEARCH INDEX grams ON docs(body) USING FULLTEXT WITH(tokenizer='ngram',min_gram=2,max_gram=4)", &crate::Parameters::new()).unwrap();
        let catalog = c.catalog("docs").unwrap();
        assert_eq!(catalog.version, 5);
        let original = serde_json::to_value(&catalog).unwrap();
        for (path, value) in [
            ("/version", serde_json::json!(4)),
            ("/indexes/0/fulltext/storage_version", serde_json::json!(1)),
            ("/indexes/0/fulltext/min_gram", serde_json::json!(0)),
            ("/indexes/0/fulltext/max_gram", serde_json::json!(65)),
            ("/indexes/0/fulltext/min_gram", serde_json::Value::Null),
            (
                "/indexes/0/fulltext/tokenizer",
                serde_json::json!("tantivy-NGRAM-0.26"),
            ),
            (
                "/indexes/0/fulltext/tokenizer",
                serde_json::json!("tantivy-default-0.26"),
            ),
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                crate::catalog::decode(&changed.to_string(), "docs").is_err(),
                "{path}"
            );
        }
        c.run("DROP INDEX grams", &[]).unwrap();
        c.run(
            &format!(
                "CREATE INDEX grams ON {} USING fts(key) WITH(tokenizer='simple')",
                crate::quote(&catalog.indexes[0].storage)
            ),
            &[],
        )
        .unwrap();
        assert!(db.connect().is_err());
    }
}
