use rustc_hash::FxHashMap as HashMap;
use tantivy::query::{Query, QueryParser, QueryParserError};
use tantivy::query_grammar::{Delimiter, Occur, UserInputAst, UserInputLeaf, UserInputLiteral};
use tantivy::schema::{Field, FieldType};
use tantivy::tokenizer::{TextAnalyzer, TokenStream};
use tantivy::{Index, Searcher};

pub const MAX_PREFIX_TERMS: usize = 64;
pub const MIN_PREFIX_CHARS: usize = 2;

pub(super) fn parse_query(
    parser: &QueryParser,
    index: &Index,
    searcher: &Searcher,
    default_fields: &[Field],
    query: &str,
) -> Result<Box<dyn Query>, String> {
    let (ast, syntax_errors) = tantivy::query_grammar::parse_query_lenient(query);
    if let Some(error) = syntax_errors.first() {
        let error =
            QueryParserError::SyntaxError(format!("{} at position {}", error.message, error.pos));
        return Err(format!("FTS parse error: {error:?}"));
    }
    let expander = Expander {
        index,
        searcher,
        default_fields,
    };
    let ast = expander.expand(ast)?;
    let (query, errors) = parser.build_query_from_user_input_ast_lenient(ast);
    match errors.first() {
        Some(error) => Err(format!("FTS parse error: {error:?}")),
        None => Ok(query),
    }
}

struct Expander<'a> {
    index: &'a Index,
    searcher: &'a Searcher,
    default_fields: &'a [Field],
}

impl Expander<'_> {
    fn expand(&self, ast: UserInputAst) -> Result<UserInputAst, String> {
        match ast {
            UserInputAst::Clause(children) => Ok(UserInputAst::Clause(
                children
                    .into_iter()
                    .map(|(occur, child)| Ok((occur, self.expand(child)?)))
                    .collect::<Result<_, String>>()?,
            )),
            UserInputAst::Boost(child, boost) => {
                Ok(UserInputAst::Boost(Box::new(self.expand(*child)?), boost))
            }
            UserInputAst::Leaf(leaf) => match *leaf {
                UserInputLeaf::Literal(literal) => match prefix_stem(&literal) {
                    Some(stem) => self.expand_prefix(literal, stem),
                    None => Ok(UserInputAst::Leaf(Box::new(UserInputLeaf::Literal(
                        literal,
                    )))),
                },
                leaf => Ok(UserInputAst::Leaf(Box::new(leaf))),
            },
        }
    }

    fn expand_prefix(
        &self,
        literal: UserInputLiteral,
        stem: String,
    ) -> Result<UserInputAst, String> {
        let schema = self.index.schema();
        let fields = match &literal.field_name {
            Some(name) => match schema.get_field(name) {
                Ok(field) => vec![field],
                Err(_) => return Ok(leaf(literal)),
            },
            None => self.default_fields.to_vec(),
        };
        let Some(&field) = fields.first() else {
            return Ok(leaf(literal));
        };
        let tokenizer = match schema.get_field_entry(field).field_type() {
            FieldType::Str(options) => match options.get_indexing_options() {
                Some(indexing) => indexing.tokenizer().to_owned(),
                None => return Ok(leaf(literal)),
            },
            _ => return Ok(leaf(literal)),
        };
        if tokenizer == "ngram" {
            return Ok(leaf(UserInputLiteral {
                phrase: stem,
                prefix: false,
                ..literal
            }));
        }
        let mut analyzer = self
            .index
            .tokenizer_for_field(field)
            .map_err(|e| format!("FTS tokenizer error: {e}"))?;
        let tokens = analyzed_tokens(&mut analyzer, &stem);
        match tokens.as_slice() {
            [] => Ok(UserInputAst::Clause(Vec::new())),
            [token] => {
                if token.chars().count() < MIN_PREFIX_CHARS {
                    return Err(format!(
                        "FTS prefix needs at least {MIN_PREFIX_CHARS} characters before *"
                    ));
                }
                let words = self.words_with_prefix(&fields, token)?;
                if words.is_empty() {
                    return Ok(leaf(UserInputLiteral {
                        phrase: token.clone(),
                        delimiter: Delimiter::None,
                        prefix: false,
                        ..literal
                    }));
                }
                Ok(UserInputAst::Clause(
                    words
                        .into_iter()
                        .map(|word| {
                            let word = UserInputLiteral {
                                field_name: literal.field_name.clone(),
                                phrase: word,
                                delimiter: Delimiter::None,
                                slop: 0,
                                prefix: false,
                            };
                            (Some(Occur::Should), leaf(word))
                        })
                        .collect(),
                ))
            }
            _ => Ok(leaf(UserInputLiteral {
                phrase: stem,
                delimiter: Delimiter::DoubleQuotes,
                slop: 0,
                prefix: true,
                ..literal
            })),
        }
    }

    fn words_with_prefix(&self, fields: &[Field], prefix: &str) -> Result<Vec<String>, String> {
        let mut doc_freqs: HashMap<Vec<u8>, u64> = HashMap::default();
        for segment in self.searcher.segment_readers() {
            for &field in fields {
                let inverted = segment
                    .inverted_index(field)
                    .map_err(|e| format!("FTS term dictionary error: {e}"))?;
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
}

fn prefix_stem(literal: &UserInputLiteral) -> Option<String> {
    if literal.prefix {
        return Some(literal.phrase.clone());
    }
    if literal.delimiter != Delimiter::None {
        return None;
    }
    let stem = literal.phrase.strip_suffix('*')?;
    (!stem.is_empty() && !stem.contains('*')).then(|| stem.to_owned())
}

fn analyzed_tokens(analyzer: &mut TextAnalyzer, text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        tokens.push(stream.token().text.clone());
    }
    tokens
}

fn leaf(literal: UserInputLiteral) -> UserInputAst {
    UserInputAst::Leaf(Box::new(UserInputLeaf::Literal(literal)))
}
