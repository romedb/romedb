use crate::LimboError;
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, NgramTokenizer, RawTokenizer, RemoveLongFilter,
    SimpleTokenizer, Stemmer, TextAnalyzer, TokenStream, TokenizerManager, WhitespaceTokenizer,
};

pub const FOLDING_TOKENIZERS: &[&str] = &["default", "simple", "whitespace"];

pub const STEMMER_LANGUAGES: &[&str] = &[
    "arabic",
    "danish",
    "dutch",
    "english",
    "finnish",
    "french",
    "german",
    "greek",
    "hungarian",
    "italian",
    "norwegian",
    "portuguese",
    "romanian",
    "russian",
    "spanish",
    "swedish",
    "tamil",
    "turkish",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Analyzer {
    tokenizer: String,
    ngram_window: (usize, usize),
    fold: bool,
    stemmer: Option<&'static str>,
}

impl Analyzer {
    pub fn new(
        tokenizer: &str,
        ngram_window: (usize, usize),
        fold: bool,
        stemmer: Option<&str>,
    ) -> Result<Self, LimboError> {
        if !super::SUPPORTED_TOKENIZERS.contains(&tokenizer) {
            return Err(LimboError::ParseError(format!(
                "unsupported FTS tokenizer '{}'. Supported tokenizers: {}",
                tokenizer,
                super::SUPPORTED_TOKENIZERS.join(", ")
            )));
        }
        if fold && !FOLDING_TOKENIZERS.contains(&tokenizer) {
            return Err(LimboError::ParseError(format!(
                "FTS WITH parameter 'fold' requires tokenizer = 'default', 'simple' or 'whitespace', got tokenizer = '{tokenizer}'"
            )));
        }
        let stemmer = match stemmer {
            None => None,
            Some(_) if tokenizer != "default" => {
                return Err(LimboError::ParseError(format!(
                    "FTS WITH parameter 'stemmer' requires tokenizer = 'default', got tokenizer = '{tokenizer}'"
                )));
            }
            Some(name) => Some(
                *STEMMER_LANGUAGES
                    .iter()
                    .find(|language| **language == name)
                    .ok_or_else(|| {
                        LimboError::ParseError(format!(
                            "unsupported FTS stemmer '{}'. Supported stemmers: {}",
                            name,
                            STEMMER_LANGUAGES.join(", ")
                        ))
                    })?,
            ),
        };
        Ok(Self {
            tokenizer: tokenizer.to_owned(),
            ngram_window,
            fold,
            stemmer,
        })
    }

    pub fn tokenizer(&self) -> &str {
        &self.tokenizer
    }

    pub fn stems(&self) -> bool {
        self.stemmer.is_some()
    }

    pub fn without_stemmer(&self) -> Self {
        Self {
            stemmer: None,
            ..self.clone()
        }
    }

    pub(super) fn name(&self) -> String {
        let mut name = self.tokenizer.clone();
        if self.fold {
            name.push_str("+fold");
        }
        if let Some(language) = self.stemmer {
            name.push_str("+stem-");
            name.push_str(language);
        }
        name
    }

    pub(super) fn register(&self, tokenizers: &TokenizerManager) {
        tokenizers.register("raw", RawTokenizer::default());
        tokenizers.register("simple", SimpleTokenizer::default());
        tokenizers.register("whitespace", WhitespaceTokenizer::default());
        if let Ok(ngram) = NgramTokenizer::new(self.ngram_window.0, self.ngram_window.1, false) {
            let analyzer = TextAnalyzer::builder(ngram).filter(LowerCaser).build();
            tokenizers.register("ngram", analyzer);
        }
        if self.fold || self.stemmer.is_some() {
            tokenizers.register(&self.name(), self.filtered());
        }
    }

    pub(super) fn text_analyzer(&self) -> Result<TextAnalyzer, LimboError> {
        let tokenizers = TokenizerManager::default();
        self.register(&tokenizers);
        tokenizers
            .get(&self.name())
            .ok_or_else(|| LimboError::ParseError("invalid FTS tokenizer configuration".into()))
    }

    fn filtered(&self) -> TextAnalyzer {
        let mut builder = match self.tokenizer.as_str() {
            "default" => TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(RemoveLongFilter::limit(40))
                .filter(LowerCaser)
                .dynamic(),
            "simple" => TextAnalyzer::builder(SimpleTokenizer::default()).dynamic(),
            "whitespace" => TextAnalyzer::builder(WhitespaceTokenizer::default()).dynamic(),
            tokenizer => unreachable!("FTS options were checked for tokenizer '{tokenizer}'"),
        };
        if self.fold {
            builder = builder.filter_dynamic(AsciiFoldingFilter);
        }
        if let Some(language) = self.stemmer {
            builder = builder.filter_dynamic(Stemmer::new(stemmer_language(language)));
        }
        builder.build()
    }
}

fn stemmer_language(name: &str) -> Language {
    match name {
        "arabic" => Language::Arabic,
        "danish" => Language::Danish,
        "dutch" => Language::Dutch,
        "english" => Language::English,
        "finnish" => Language::Finnish,
        "french" => Language::French,
        "german" => Language::German,
        "greek" => Language::Greek,
        "hungarian" => Language::Hungarian,
        "italian" => Language::Italian,
        "norwegian" => Language::Norwegian,
        "portuguese" => Language::Portuguese,
        "romanian" => Language::Romanian,
        "russian" => Language::Russian,
        "spanish" => Language::Spanish,
        "swedish" => Language::Swedish,
        "tamil" => Language::Tamil,
        "turkish" => Language::Turkish,
        name => unreachable!("FTS stemmer '{name}' was checked against STEMMER_LANGUAGES"),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AnalyzedToken<'a> {
    pub text: &'a str,
    pub offset_from: usize,
    pub offset_to: usize,
    pub position: usize,
    pub position_length: usize,
}

pub fn analyze_text<E: From<LimboError>>(
    analyzer: &Analyzer,
    text: &str,
    mut visit: impl FnMut(AnalyzedToken<'_>) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let mut analyzer = analyzer.text_analyzer()?;
    let mut stream = analyzer.token_stream(text);
    while stream.advance() {
        let token = stream.token();
        visit(AnalyzedToken {
            text: &token.text,
            offset_from: token.offset_from,
            offset_to: token.offset_to,
            position: token.position,
            position_length: token.position_length,
        })?;
    }
    Ok(())
}
