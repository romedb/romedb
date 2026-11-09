use crate::analyzer::FullTextOptions;
use crate::{Document, Error, Result, Value};
use std::ops::Range;
use tantivy_query_grammar::{Delimiter, Occur, UserInputAst, UserInputLeaf, UserInputLiteral};

const MIN_SNIPPET_SIZE: usize = 40;
const MAX_SNIPPET_SIZE: usize = 1000;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_WORDS: usize = 64;
const MAX_FOUND_TOKENS: usize = 65_536;
const MAX_MATCHES: usize = 10_000;
const ELLIPSIS: &str = "\u{2026}";

pub(crate) fn call(arguments: &[Value]) -> Result<Value> {
    match arguments {
        [Value::String(config), Value::String(_index), text, query, size] => {
            let options: FullTextOptions = serde_json::from_str(config).map_err(|error| {
                Error::Storage(format!("invalid analyzer configuration: {error}"))
            })?;
            snippet_value(&options, text, query, size)
        }
        _ => Err(wrong_arguments()),
    }
}

impl crate::Connection {
    pub fn snippet_text(&self, index: &str, text: &str, query: &str, size: usize) -> Result<Value> {
        let size = i64::try_from(size).unwrap_or(i64::MAX);
        self.snippet_values(
            index,
            &Value::String(text.into()),
            &Value::String(query.into()),
            &Value::Integer(size),
        )
    }
    pub(crate) fn snippet_values(
        &self,
        index: &str,
        text: &Value,
        query: &Value,
        size: &Value,
    ) -> Result<Value> {
        self.atomic(|| {
            let options = self.analyzer_options(index, "search::snippet")?;
            snippet_value(&options, text, query, size)
        })
    }
}

fn snippet_value(
    options: &FullTextOptions,
    text: &Value,
    query: &Value,
    size: &Value,
) -> Result<Value> {
    let size = match size {
        Value::Integer(size)
            if (MIN_SNIPPET_SIZE as i64..=MAX_SNIPPET_SIZE as i64).contains(size) =>
        {
            *size as usize
        }
        _ => return Err(size_error()),
    };
    let query = match query {
        Value::String(query) => query,
        Value::Null => return Ok(Value::Null),
        _ => {
            return Err(Error::Validation(
                "search::snippet expects a text query".into(),
            ))
        }
    };
    crate::fulltext::validate_query(query)?;
    match text {
        Value::String(text) => snippet(options, text, query, size),
        Value::Null => Ok(Value::Null),
        _ => Err(Error::Validation(
            "search::snippet expects text or null".into(),
        )),
    }
}

fn size_error() -> Error {
    Error::Validation(format!(
        "search::snippet size must be an integer from {MIN_SNIPPET_SIZE} to {MAX_SNIPPET_SIZE}"
    ))
}

fn wrong_arguments() -> Error {
    Error::Validation("search::snippet expects an index name, text, query and size".into())
}

fn snippet(options: &FullTextOptions, text: &str, query: &str, size: usize) -> Result<Value> {
    let options = options.normalized()?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::Limit("snippet text exceeds 1 MiB".into()));
    }
    let phrases = query_phrases(&options, query)?;
    let mut matches = if options.tokenizer == "ngram" {
        substring_matches(text, &phrases)
    } else {
        token_matches(&options, text, &phrases)?
    };
    matches.sort_by_key(|found| (found.from, found.to));
    let fragment = best_fragment(text, &matches, size);
    Ok(parts(text, fragment, &matches))
}

struct Phrase {
    source: String,
    words: Vec<(usize, String)>,
    prefix: bool,
}

struct Match {
    from: usize,
    to: usize,
    phrase: usize,
}

fn query_phrases(options: &FullTextOptions, query: &str) -> Result<Vec<Phrase>> {
    let ast = tantivy_query_grammar::parse_query(query)
        .map_err(|_| Error::Validation("invalid text query syntax".into()))?;
    let mut literals = Vec::new();
    positive_literals(&ast, &mut literals);
    let ngram = options.tokenizer == "ngram";
    let mut phrases = Vec::new();
    for literal in literals {
        let (source, prefix) = literal_source(literal, ngram);
        let words = analyzed_words(options, source)?;
        if words.is_empty() {
            continue;
        }
        if literal.slop == 0 || ngram {
            phrases.push(Phrase {
                source: source.to_owned(),
                words,
                prefix,
            });
            continue;
        }
        let last = words.len() - 1;
        phrases.extend(
            words
                .into_iter()
                .enumerate()
                .map(|(index, (_, word))| Phrase {
                    source: word.clone(),
                    words: vec![(0, word)],
                    prefix: prefix && index == last,
                }),
        );
    }
    phrases.truncate(MAX_WORDS);
    Ok(phrases)
}

fn positive_literals<'a>(ast: &'a UserInputAst, literals: &mut Vec<&'a UserInputLiteral>) {
    match ast {
        UserInputAst::Clause(children) => {
            for (occur, child) in children {
                if *occur != Some(Occur::MustNot) {
                    positive_literals(child, literals);
                }
            }
        }
        UserInputAst::Leaf(leaf) => {
            if let UserInputLeaf::Literal(literal) = leaf.as_ref() {
                literals.push(literal);
            }
        }
        UserInputAst::Boost(child, _) => positive_literals(child, literals),
    }
}

fn literal_source(literal: &UserInputLiteral, ngram: bool) -> (&str, bool) {
    let stem = match literal.delimiter {
        Delimiter::None => literal
            .phrase
            .strip_suffix('*')
            .filter(|stem| !stem.is_empty()),
        _ => None,
    };
    match stem {
        Some(stem) => (stem, !ngram),
        None => (literal.phrase.as_str(), literal.prefix && !ngram),
    }
}

fn analyzed_words(options: &FullTextOptions, text: &str) -> Result<Vec<(usize, String)>> {
    let mut words: Vec<(usize, String)> = Vec::new();
    analyze(options, text, |token| {
        words.push((token.position, token.text.to_owned()));
    })?;
    let first = words.first().map_or(0, |(position, _)| *position);
    Ok(words
        .into_iter()
        .map(|(position, word)| (position - first, word))
        .collect())
}

fn analyze(
    options: &FullTextOptions,
    text: &str,
    mut visit: impl FnMut(turso_core::index_method::fts::AnalyzedToken<'_>),
) -> Result<()> {
    turso_core::index_method::fts::analyze_text::<Error>(
        &options.tokenizer,
        (options.min_gram.unwrap_or(2), options.max_gram.unwrap_or(3)),
        text,
        |token| {
            visit(token);
            Ok(())
        },
    )
}

struct FoundToken {
    from: usize,
    to: usize,
    position: usize,
    words: u64,
}

fn token_matches(options: &FullTextOptions, text: &str, phrases: &[Phrase]) -> Result<Vec<Match>> {
    let mut words: Vec<(&str, bool)> = Vec::new();
    let mut phrase_words: Vec<Vec<(usize, usize)>> = Vec::new();
    for phrase in phrases {
        let last = phrase.words.len() - 1;
        let mut entries = Vec::new();
        for (index, (position, word)) in phrase.words.iter().enumerate() {
            let key = (word.as_str(), phrase.prefix && index == last);
            let entry = match words.iter().position(|known| *known == key) {
                Some(entry) => entry,
                None if words.len() < MAX_WORDS => {
                    words.push(key);
                    words.len() - 1
                }
                None => break,
            };
            entries.push((*position, entry));
        }
        if entries.len() == phrase.words.len() {
            phrase_words.push(entries);
        } else {
            phrase_words.push(Vec::new());
        }
    }
    let mut found: Vec<FoundToken> = Vec::new();
    analyze(options, text, |token| {
        if found.len() >= MAX_FOUND_TOKENS {
            return;
        }
        let matched = words
            .iter()
            .enumerate()
            .filter(|(_, (word, prefix))| {
                if *prefix {
                    token.text.starts_with(word)
                } else {
                    token.text == *word
                }
            })
            .fold(0u64, |bits, (entry, _)| bits | (1 << entry));
        if matched != 0 {
            found.push(FoundToken {
                from: token.offset_from,
                to: token.offset_to,
                position: token.position,
                words: matched,
            });
        }
    })?;
    let mut matches = Vec::new();
    for (phrase, entries) in phrase_words.iter().enumerate() {
        let Some(&(_, first_entry)) = entries.first() else {
            continue;
        };
        for start in found
            .iter()
            .filter(|token| token.words & (1 << first_entry) != 0)
        {
            let mut to = start.to;
            let complete = entries[1..].iter().all(|&(offset, entry)| {
                match token_at(&found, start.position + offset, entry) {
                    Some(token) => {
                        to = to.max(token.to);
                        true
                    }
                    None => false,
                }
            });
            if complete {
                matches.push(Match {
                    from: start.from,
                    to,
                    phrase,
                });
                if matches.len() >= MAX_MATCHES {
                    return Ok(matches);
                }
            }
        }
    }
    Ok(matches)
}

fn token_at(found: &[FoundToken], position: usize, entry: usize) -> Option<&FoundToken> {
    let first = found.partition_point(|token| token.position < position);
    found[first..]
        .iter()
        .take_while(|token| token.position == position)
        .find(|token| token.words & (1 << entry) != 0)
}

fn substring_matches(text: &str, phrases: &[Phrase]) -> Vec<Match> {
    let lowered = Lowered::new(text);
    let mut matches = Vec::new();
    for (phrase, query) in phrases.iter().enumerate() {
        let needle: String = query.source.chars().flat_map(char::to_lowercase).collect();
        for (at, _) in lowered.text.match_indices(&needle) {
            let (Some(from), Some(to)) = (lowered.source(at), lowered.source(at + needle.len()))
            else {
                continue;
            };
            matches.push(Match { from, to, phrase });
            if matches.len() >= MAX_MATCHES {
                return matches;
            }
        }
    }
    matches
}

struct Lowered {
    text: String,
    starts: Option<Vec<(u32, u32)>>,
}

impl Lowered {
    fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Self {
                text: text.to_ascii_lowercase(),
                starts: None,
            };
        }
        let mut lowered = String::with_capacity(text.len());
        let mut starts = Vec::with_capacity(text.len() + 1);
        for (offset, c) in text.char_indices() {
            starts.push((lowered.len() as u32, offset as u32));
            lowered.extend(c.to_lowercase());
        }
        starts.push((lowered.len() as u32, text.len() as u32));
        Self {
            text: lowered,
            starts: Some(starts),
        }
    }
    fn source(&self, offset: usize) -> Option<usize> {
        let Some(starts) = &self.starts else {
            return Some(offset);
        };
        let index = starts
            .binary_search_by_key(&(offset as u32), |(lowered, _)| *lowered)
            .ok()?;
        Some(starts[index].1 as usize)
    }
}

fn best_fragment(text: &str, matches: &[Match], size: usize) -> Range<usize> {
    let Some(window) = best_window(text, matches, size) else {
        let end = char_end(text, 0, size);
        return match word_end_before(text, end, 0) {
            0 => 0..end,
            word_end => 0..word_end,
        };
    };
    let used = text[window.clone()].chars().count();
    if used >= size {
        return window.start..char_end(text, window.start, size);
    }
    if text[..window.end].chars().count() <= size {
        return 0..word_end_before(text, char_end(text, 0, size), window.end);
    }
    let room = size - used;
    let mut before = (size * 3 / 10).min(room);
    let after = room - before;
    let after_available = text[window.end..].chars().take(after).count();
    before += after - after_available;
    let from = char_start(text, window.start, before);
    let to = char_end(text, window.end, after_available);
    word_start_after(text, from, window.start)..word_end_before(text, to, window.end)
}

fn best_window(text: &str, matches: &[Match], size: usize) -> Option<Range<usize>> {
    let mut spans = Vec::with_capacity(matches.len());
    let (mut byte, mut char) = (0, 0);
    for found in matches {
        char += text[byte..found.from].chars().count();
        byte = found.from;
        spans.push((char, char + text[found.from..found.to].chars().count()));
    }
    let mut best: Option<((u32, usize), Range<usize>)> = None;
    for (first, &(start, end)) in spans.iter().enumerate() {
        let mut phrases = 0u64;
        let mut count = 0;
        let mut window = matches[first].from..matches[first].to;
        if end - start <= size {
            for (found, &(from, to)) in matches[first..].iter().zip(&spans[first..]) {
                if from >= start + size {
                    break;
                }
                if to - start > size {
                    continue;
                }
                phrases |= 1 << found.phrase;
                count += 1;
                window.end = window.end.max(found.to);
            }
        } else {
            phrases = 1 << matches[first].phrase;
            count = 1;
        }
        let score = (phrases.count_ones(), count);
        if best.as_ref().is_none_or(|(best, _)| score > *best) {
            best = Some((score, window));
        }
    }
    best.map(|(_, window)| window)
}

fn char_end(text: &str, from: usize, chars: usize) -> usize {
    text[from..]
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(offset, _)| from + offset)
}

fn char_start(text: &str, to: usize, chars: usize) -> usize {
    if chars == 0 {
        return to;
    }
    text[..to]
        .char_indices()
        .rev()
        .nth(chars - 1)
        .map_or(0, |(offset, _)| offset)
}

fn word_start_after(text: &str, from: usize, limit: usize) -> usize {
    if from == 0 {
        return 0;
    }
    let mut previous = text[..from].chars().next_back();
    for (offset, c) in text[from..limit].char_indices() {
        if c.is_alphanumeric() && !previous.is_some_and(char::is_alphanumeric) {
            return from + offset;
        }
        previous = Some(c);
    }
    if !previous.is_some_and(char::is_alphanumeric) {
        return limit;
    }
    from
}

fn word_end_before(text: &str, to: usize, limit: usize) -> usize {
    if to == text.len() {
        return to;
    }
    let mut next = text[to..].chars().next();
    for (offset, c) in text[limit..to].char_indices().rev() {
        if c.is_alphanumeric() && !next.is_some_and(char::is_alphanumeric) {
            return limit + offset + c.len_utf8();
        }
        next = Some(c);
    }
    if !next.is_some_and(char::is_alphanumeric) {
        return limit;
    }
    to
}

fn parts(text: &str, fragment: Range<usize>, matches: &[Match]) -> Value {
    let mut marked: Vec<Range<usize>> = Vec::new();
    for found in matches {
        if found.from < fragment.start || found.to > fragment.end {
            continue;
        }
        match marked.last_mut() {
            Some(last) if found.from <= last.end => last.end = last.end.max(found.to),
            _ => marked.push(found.from..found.to),
        }
    }
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut at = fragment.start;
    for range in marked {
        parts.push((single_spaced(&text[at..range.start]), false));
        parts.push((single_spaced(&text[range.clone()]), true));
        at = range.end;
    }
    parts.push((single_spaced(&text[at..fragment.end]), false));
    if let Some((first, false)) = parts.first_mut() {
        *first = first.trim_start().to_owned();
    }
    if let Some((last, false)) = parts.last_mut() {
        *last = last.trim_end().to_owned();
    }
    parts.retain(|(part, _)| !part.is_empty());
    if parts.is_empty() {
        return Value::Array(Vec::new());
    }
    if text[..fragment.start].chars().any(char::is_alphanumeric) {
        match parts.first_mut() {
            Some((first, false)) => first.insert_str(0, ELLIPSIS),
            _ => parts.insert(0, (ELLIPSIS.into(), false)),
        }
    }
    if text[fragment.end..].chars().any(char::is_alphanumeric) {
        match parts.last_mut() {
            Some((last, false)) => last.push_str(ELLIPSIS),
            _ => parts.push((ELLIPSIS.into(), false)),
        }
    }
    Value::Array(
        parts
            .into_iter()
            .map(|(part, matched)| {
                Value::Object(Document::from([
                    ("text".into(), Value::String(part)),
                    ("match".into(), Value::Boolean(matched)),
                ]))
            })
            .collect(),
    )
}

fn single_spaced(text: &str) -> String {
    let mut spaced = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space {
            spaced.push(' ');
            space = false;
        }
        spaced.push(c);
    }
    if space {
        spaced.push(' ');
    }
    spaced
}
