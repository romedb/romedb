use super::Analyzer;
use rustc_hash::FxHashMap as HashMap;
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, EmptyQuery, FuzzyTermQuery, Query, QueryParser,
    QueryParserError, TermQuery, TermSetQuery,
};
use tantivy::query_grammar::{Delimiter, Occur, UserInputAst, UserInputLeaf, UserInputLiteral};
use tantivy::schema::{Field, FieldType, IndexRecordOption};
use tantivy::tokenizer::{TextAnalyzer, TokenStream};
use tantivy::{Index, Score, Searcher, Term};

pub const MAX_PREFIX_TERMS: usize = 64;
pub const MIN_PREFIX_CHARS: usize = 2;
pub const MAX_TYPO_WORDS: usize = 64;
pub const MIN_ONE_TYPO_CHARS: usize = 4;
pub const MIN_TWO_TYPO_CHARS: usize = 8;
const TYPO_SCORE_FACTOR: Score = 0.5;

pub(super) struct Expander<'a> {
    pub parser: &'a QueryParser,
    pub index: &'a Index,
    pub searcher: &'a Searcher,
    pub analyzer: &'a Analyzer,
    pub default_fields: &'a [Field],
    pub filter_fields: &'a [Field],
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
        if !self.needs_own_build(&ast) {
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
                UserInputLeaf::Literal(literal) => {
                    if let Some(field) = self.filter_field(literal.field_name.as_deref()) {
                        return filter_value_query(field, literal).map(Some);
                    }
                    if let Some(text) = prefix_text(&literal) {
                        return self.prefix_query(literal, text);
                    }
                    if let Some((text, distance)) = typo_text(&literal) {
                        return self.typo_query(literal, text, distance);
                    }
                    self.leaf_query(UserInputLeaf::Literal(literal))
                }
                UserInputLeaf::Set { field, elements } => match self.filter_field(field.as_deref())
                {
                    Some(field) => Ok(Some(filter_set_query(field, elements))),
                    None => self.leaf_query(UserInputLeaf::Set { field, elements }),
                },
                leaf => self.leaf_query(leaf),
            },
        }
    }

    fn needs_own_build(&self, ast: &UserInputAst) -> bool {
        match ast {
            UserInputAst::Clause(children) => children
                .iter()
                .any(|(_, child)| self.needs_own_build(child)),
            UserInputAst::Boost(child, _) => self.needs_own_build(child),
            UserInputAst::Leaf(leaf) => match leaf.as_ref() {
                UserInputLeaf::Literal(literal) => {
                    prefix_text(literal).is_some()
                        || typo_text(literal).is_some()
                        || self.filter_field(literal.field_name.as_deref()).is_some()
                }
                UserInputLeaf::Set { field, .. } => self.filter_field(field.as_deref()).is_some(),
                _ => false,
            },
        }
    }

    fn filter_field(&self, name: Option<&str>) -> Option<Field> {
        let field = self.index.schema().get_field(name?).ok()?;
        self.filter_fields.contains(&field).then_some(field)
    }

    fn prefix_query(
        &self,
        literal: UserInputLiteral,
        text: String,
    ) -> Result<Option<Box<dyn Query>>, String> {
        let Some(fields) = self.text_fields(&literal) else {
            return self.leaf_query(UserInputLeaf::Literal(literal));
        };
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

    fn typo_query(
        &self,
        literal: UserInputLiteral,
        text: String,
        asked: u8,
    ) -> Result<Option<Box<dyn Query>>, String> {
        let fields = self.text_fields(&literal);
        let exact = UserInputLeaf::Literal(UserInputLiteral {
            phrase: text.clone(),
            ..literal
        });
        let Some(fields) = fields.filter(|_| self.analyzer.tokenizer() != "ngram") else {
            return self.leaf_query(exact);
        };
        let typed = analyzed_tokens(&self.analyzer.without_stemmer(), &text)?;
        let words = analyzed_tokens(self.analyzer, &text)?;
        let ([typed], [word]) = (typed.as_slice(), words.as_slice()) else {
            return self.leaf_query(exact);
        };
        let distance = allowed_typos(asked, typed.chars().count());
        if distance == 0 {
            return self.leaf_query(exact);
        }
        let near = self.words_near(&fields, word, distance)?;
        self.typo_words_query(&fields, word, near).map(Some)
    }

    fn text_fields(&self, literal: &UserInputLiteral) -> Option<Vec<Field>> {
        let schema = self.index.schema();
        let fields = match &literal.field_name {
            Some(name) => vec![schema.get_field(name).ok()?],
            None => self.default_fields.to_vec(),
        };
        let indexed_text = |field: &Field| match schema.get_field_entry(*field).field_type() {
            FieldType::Str(options) => options.get_indexing_options().is_some(),
            _ => false,
        };
        (!fields.is_empty() && fields.iter().all(indexed_text)).then_some(fields)
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
        Ok(most_common(doc_freqs, MAX_PREFIX_TERMS))
    }

    fn words_near(
        &self,
        fields: &[Field],
        word: &str,
        distance: u8,
    ) -> Result<Vec<String>, String> {
        let mut doc_freqs: HashMap<Vec<u8>, u64> = HashMap::default();
        for segment in self.searcher.segment_readers() {
            for &field in fields {
                let inverted = segment
                    .inverted_index(field)
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                let automaton =
                    FuzzyTermQuery::new(Term::from_field_text(field, word), distance, true)
                        .automaton()
                        .map_err(|e| format!("FTS typo error: {e}"))?;
                let mut stream = inverted
                    .terms()
                    .search(automaton)
                    .into_stream()
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
                while stream.advance() {
                    if stream.key() != word.as_bytes() {
                        *doc_freqs.entry(stream.key().to_vec()).or_default() +=
                            u64::from(stream.value().doc_freq);
                    }
                }
            }
        }
        Ok(most_common(doc_freqs, MAX_TYPO_WORDS))
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

    fn typo_words_query(
        &self,
        fields: &[Field],
        word: &str,
        near: Vec<String>,
    ) -> Result<Box<dyn Query>, String> {
        let doc_count: u64 = self
            .searcher
            .segment_readers()
            .iter()
            .map(|segment| u64::from(segment.max_doc()))
            .sum();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for &field in fields {
            let doc_freq = |word: &str| {
                self.searcher
                    .doc_freq(&Term::from_field_text(field, word))
                    .map_err(|e| format!("FTS term dictionary error: {e}"))
            };
            let exact = doc_freq(word)?;
            let mut found = Vec::with_capacity(near.len());
            for other in &near {
                found.push((other, doc_freq(other)?));
            }
            let most = found.iter().map(|(_, freq)| *freq).fold(exact, u64::max);
            let term = |word: &str, boost: Score| {
                let query = TermQuery::new(
                    Term::from_field_text(field, word),
                    IndexRecordOption::WithFreqs,
                );
                (Occur::Should, boosted(Box::new(query), boost))
            };
            if exact > 0 {
                clauses.push(term(word, self.field_boost(field)));
            }
            for (other, freq) in found {
                if freq > 0 {
                    let scale = TYPO_SCORE_FACTOR * idf(most, doc_count) / idf(freq, doc_count);
                    clauses.push(term(other, self.field_boost(field) * scale));
                }
            }
        }
        if clauses.is_empty() {
            return Ok(Box::new(EmptyQuery));
        }
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    fn field_boost(&self, field: Field) -> Score {
        self.field_boosts
            .iter()
            .find(|(boosted, _)| *boosted == field)
            .map_or(1.0, |(_, boost)| *boost)
    }
}

fn filter_value_query(field: Field, literal: UserInputLiteral) -> Result<Box<dyn Query>, String> {
    if prefix_text(&literal).is_some() || literal.slop != 0 {
        return Err(format!(
            "FTS filter column '{}' takes a whole value, not a prefix or a phrase slop",
            literal.field_name.unwrap_or_default()
        ));
    }
    let term = TermQuery::new(
        Term::from_field_text(field, &literal.phrase),
        IndexRecordOption::Basic,
    );
    Ok(Box::new(ConstScoreQuery::new(Box::new(term), 0.0)))
}

fn filter_set_query(field: Field, values: Vec<String>) -> Box<dyn Query> {
    let terms = values
        .iter()
        .map(|value| Term::from_field_text(field, value));
    Box::new(ConstScoreQuery::new(
        Box::new(TermSetQuery::new(terms)),
        0.0,
    ))
}

fn most_common(doc_freqs: HashMap<Vec<u8>, u64>, count: usize) -> Vec<String> {
    let mut words: Vec<(u64, Vec<u8>)> = doc_freqs
        .into_iter()
        .map(|(word, doc_freq)| (doc_freq, word))
        .collect();
    words.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    words
        .into_iter()
        .filter_map(|(_, word)| String::from_utf8(word).ok())
        .take(count)
        .collect()
}

fn idf(doc_freq: u64, doc_count: u64) -> Score {
    let rest = (doc_count.saturating_sub(doc_freq)) as Score + 0.5;
    (1.0 + rest / (doc_freq as Score + 0.5)).ln()
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

fn typo_text(literal: &UserInputLiteral) -> Option<(String, u8)> {
    if literal.delimiter != Delimiter::None || literal.prefix || literal.slop != 0 {
        return None;
    }
    typo_word(&literal.phrase).map(|(word, distance)| (word.to_owned(), distance))
}

pub fn typo_word(text: &str) -> Option<(&str, u8)> {
    let (word, distance) = text.rsplit_once('~')?;
    let distance = match distance {
        "1" => 1,
        "2" => 2,
        _ => return None,
    };
    (!word.is_empty() && !word.contains(['~', '*'])).then_some((word, distance))
}

pub fn allowed_typos(asked: u8, chars: usize) -> u8 {
    if chars >= MIN_TWO_TYPO_CHARS {
        asked
    } else if chars >= MIN_ONE_TYPO_CHARS {
        asked.min(1)
    } else {
        0
    }
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
