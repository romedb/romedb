use super::Analyzer;
use rustc_hash::FxHashMap as HashMap;
use tantivy::query::{
    BooleanQuery, BoostQuery, EmptyQuery, Query, QueryParser, QueryParserError, TermQuery,
};
use tantivy::query_grammar::{Delimiter, Occur, UserInputAst, UserInputLeaf, UserInputLiteral};
use tantivy::schema::{Field, FieldType, IndexRecordOption};
use tantivy::tokenizer::{TextAnalyzer, TokenStream};
use tantivy::{Index, Score, Searcher, Term};

pub const MAX_PREFIX_TERMS: usize = 64;
pub const MIN_PREFIX_CHARS: usize = 2;

pub(super) struct Expander<'a> {
    pub parser: &'a QueryParser,
    pub index: &'a Index,
    pub searcher: &'a Searcher,
    pub analyzer: &'a Analyzer,
    pub default_fields: &'a [Field],
    pub field_boosts: &'a [(Field, f32)],
}

impl Expander<'_> {
    pub(super) fn parse_query(&self, query: &str) -> Result<Box<dyn Query>, String> {
        let (ast, syntax_errors) = tantivy::query_grammar::parse_query_lenient(query);
        if let Some(error) = syntax_errors.first() {
            let error = QueryParserError::SyntaxError(format!(
                "{} at position {}",
                error.message, error.pos
            ));
            return Err(format!("FTS parse error: {error:?}"));
        }
        if !has_prefix(&ast) {
            let (query, errors) = self.parser.build_query_from_user_input_ast_lenient(ast);
            return match errors.first() {
                Some(error) => Err(format!("FTS parse error: {error:?}")),
                None => Ok(query),
            };
        }
        if all_negative(&ast) {
            let error = QueryParserError::AllButQueryForbidden;
            return Err(format!("FTS parse error: {error:?}"));
        }
        Ok(self.build(ast)?.unwrap_or_else(|| Box::new(EmptyQuery)))
    }

    fn build(&self, ast: UserInputAst) -> Result<Option<Box<dyn Query>>, String> {
        match ast {
            UserInputAst::Clause(children) => {
                let mut clauses = Vec::with_capacity(children.len());
                for (occur, child) in children {
                    if let Some(query) = self.build(child)? {
                        clauses.push((occur.unwrap_or(Occur::Should), query));
                    }
                }
                Ok((!clauses.is_empty())
                    .then(|| Box::new(BooleanQuery::new(clauses)) as Box<dyn Query>))
            }
            UserInputAst::Boost(child, boost) => Ok(self
                .build(*child)?
                .map(|query| boosted(query, boost.into_inner() as Score))),
            UserInputAst::Leaf(leaf) => match *leaf {
                UserInputLeaf::Literal(literal) => match prefix_text(&literal) {
                    Some(text) => self.prefix_query(literal, text),
                    None => self.leaf_query(UserInputLeaf::Literal(literal)),
                },
                leaf => self.leaf_query(leaf),
            },
        }
    }

    fn prefix_query(
        &self,
        literal: UserInputLiteral,
        text: String,
    ) -> Result<Option<Box<dyn Query>>, String> {
        let schema = self.index.schema();
        let fields = match &literal.field_name {
            Some(name) => match schema.get_field(name) {
                Ok(field) => vec![field],
                Err(_) => return self.leaf_query(UserInputLeaf::Literal(literal)),
            },
            None => self.default_fields.to_vec(),
        };
        let indexed_text = |field: &Field| match schema.get_field_entry(*field).field_type() {
            FieldType::Str(options) => options.get_indexing_options().is_some(),
            _ => false,
        };
        if fields.is_empty() || !fields.iter().all(indexed_text) {
            return self.leaf_query(UserInputLeaf::Literal(literal));
        }
        if self.analyzer.tokenizer() == "ngram" {
            return self.leaf_query(UserInputLeaf::Literal(UserInputLiteral {
                phrase: text,
                prefix: false,
                ..literal
            }));
        }
        let tokens = analyzed_tokens(&self.analyzer.without_stemmer(), &text)?;
        match tokens.as_slice() {
            [] => Ok(None),
            [token] => {
                if token.chars().count() < MIN_PREFIX_CHARS {
                    return Err(format!(
                        "FTS prefix needs at least {MIN_PREFIX_CHARS} characters before *"
                    ));
                }
                let mut prefixes = vec![token.clone()];
                if self.analyzer.stems() {
                    if let [stem] = analyzed_tokens(self.analyzer, &text)?.as_slice() {
                        prefixes.push(stem.clone());
                    }
                }
                let mut words = self.words_with_prefixes(&fields, prefixes)?;
                if words.is_empty() && self.analyzer.stems() {
                    words.extend(self.longest_word_before(&fields, token)?);
                }
                if words.is_empty() {
                    return Ok(Some(Box::new(EmptyQuery)));
                }
                Ok(Some(self.words_query(&fields, words)))
            }
            _ => self.leaf_query(UserInputLeaf::Literal(UserInputLiteral {
                phrase: text,
                delimiter: Delimiter::DoubleQuotes,
                slop: 0,
                prefix: true,
                ..literal
            })),
        }
    }

    fn leaf_query(&self, leaf: UserInputLeaf) -> Result<Option<Box<dyn Query>>, String> {
        let (query, errors) = self
            .parser
            .build_query_from_user_input_ast_lenient(UserInputAst::Leaf(Box::new(leaf)));
        if let Some(error) = errors.first() {
            return Err(format!("FTS parse error: {error:?}"));
        }
        Ok((!query.is::<EmptyQuery>()).then_some(query))
    }

    fn words_with_prefixes(
        &self,
        fields: &[Field],
        mut prefixes: Vec<String>,
    ) -> Result<Vec<String>, String> {
        prefixes.sort();
        let mut distinct: Vec<String> = Vec::with_capacity(prefixes.len());
        for prefix in prefixes {
            if !distinct
                .iter()
                .any(|kept| prefix.starts_with(kept.as_str()))
            {
                distinct.push(prefix);
            }
        }
        let mut doc_freqs: HashMap<Vec<u8>, u64> = HashMap::default();
        for segment in self.searcher.segment_readers() {
            for &field in fields {
                let inverted = segment
                    .inverted_index(field)
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                for prefix in &distinct {
                    let mut stream = inverted
                        .terms()
                        .range()
                        .ge(prefix.as_bytes())
                        .into_stream()
                        .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                    while stream.advance() {
                        let word = stream.key();
                        if !word.starts_with(prefix.as_bytes()) {
                            break;
                        }
                        *doc_freqs.entry(word.to_vec()).or_default() +=
                            u64::from(stream.value().doc_freq);
                    }
                }
            }
        }
        let mut words: Vec<(u64, Vec<u8>)> = doc_freqs
            .into_iter()
            .map(|(word, doc_freq)| (doc_freq, word))
            .collect();
        words.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(words
            .into_iter()
            .filter_map(|(_, word)| String::from_utf8(word).ok())
            .take(MAX_PREFIX_TERMS)
            .collect())
    }

    fn longest_word_before(&self, fields: &[Field], text: &str) -> Result<Option<String>, String> {
        let chars = text.chars().count();
        let shortest = MIN_PREFIX_CHARS.max(chars.div_ceil(2));
        for length in (shortest..chars).rev() {
            let (end, _) = text
                .char_indices()
                .nth(length)
                .expect("length is below the character count");
            let word = &text[..end];
            if self.has_word(fields, word)? {
                return Ok(Some(word.to_owned()));
            }
        }
        Ok(None)
    }

    fn has_word(&self, fields: &[Field], word: &str) -> Result<bool, String> {
        for segment in self.searcher.segment_readers() {
            for &field in fields {
                let inverted = segment
                    .inverted_index(field)
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                let info = inverted
                    .terms()
                    .get(word.as_bytes())
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                if info.is_some_and(|info| info.doc_freq > 0) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn words_query(&self, fields: &[Field], words: Vec<String>) -> Box<dyn Query> {
        let clauses = words
            .into_iter()
            .map(|word| {
                let mut per_field: Vec<Box<dyn Query>> = fields
                    .iter()
                    .map(|&field| {
                        let term = TermQuery::new(
                            Term::from_field_text(field, &word),
                            IndexRecordOption::WithFreqs,
                        );
                        boosted(Box::new(term), self.field_boost(field))
                    })
                    .collect();
                let query: Box<dyn Query> = if per_field.len() == 1 {
                    per_field.pop().expect("one field")
                } else {
                    Box::new(BooleanQuery::new(
                        per_field
                            .into_iter()
                            .map(|query| (Occur::Should, query))
                            .collect(),
                    ))
                };
                (Occur::Should, query)
            })
            .collect();
        Box::new(BooleanQuery::new(clauses))
    }

    fn field_boost(&self, field: Field) -> Score {
        self.field_boosts
            .iter()
            .find(|(boosted, _)| *boosted == field)
            .map_or(1.0, |(_, boost)| *boost)
    }
}

fn has_prefix(ast: &UserInputAst) -> bool {
    match ast {
        UserInputAst::Clause(children) => children.iter().any(|(_, child)| has_prefix(child)),
        UserInputAst::Boost(child, _) => has_prefix(child),
        UserInputAst::Leaf(leaf) => match leaf.as_ref() {
            UserInputLeaf::Literal(literal) => prefix_text(literal).is_some(),
            _ => false,
        },
    }
}

fn all_negative(ast: &UserInputAst) -> bool {
    match ast {
        UserInputAst::Leaf(_) => false,
        UserInputAst::Boost(child, _) => all_negative(child),
        UserInputAst::Clause(children) => children
            .iter()
            .all(|(occur, child)| *occur == Some(Occur::MustNot) || all_negative(child)),
    }
}

fn prefix_text(literal: &UserInputLiteral) -> Option<String> {
    if literal.prefix {
        return Some(literal.phrase.clone());
    }
    if literal.delimiter != Delimiter::None {
        return None;
    }
    let text = literal.phrase.strip_suffix('*')?;
    (!text.is_empty() && !text.contains('*')).then(|| text.to_owned())
}

fn analyzed_tokens(analyzer: &Analyzer, text: &str) -> Result<Vec<String>, String> {
    let mut analyzer: TextAnalyzer = analyzer.text_analyzer().map_err(|e| e.to_string())?;
    let mut tokens = Vec::new();
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    Ok(tokens)
}

fn boosted(query: Box<dyn Query>, boost: Score) -> Box<dyn Query> {
    if (boost - 1.0).abs() < Score::EPSILON {
        query
    } else {
        Box::new(BoostQuery::new(query, boost))
    }
}
