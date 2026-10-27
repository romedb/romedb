//! Recognize documented future statement forms without reserving SQL names.
use crate::{Error, Result};
use romeql_parser::Kind;

pub(crate) fn check(sql: &str) -> Result<()> {
    let tokens = romeql_parser::tokenize(sql)?;
    let prefix = |words: &[&str]| {
        words.iter().enumerate().all(|(i, word)| {
            tokens
                .get(i)
                .is_some_and(|t| t.kind == Kind::Word && t.text.eq_ignore_ascii_case(word))
        })
    };
    let feature = if prefix(&["DEFINE", "CHANGEFEED"])
        || prefix(&["REMOVE", "CHANGEFEED"])
        || prefix(&["SHOW", "CHANGES"])
    {
        Some("changefeeds are deferred to V3")
    } else if (prefix(&["LET"]) && tokens.get(1).is_some_and(|t| t.kind == Kind::Parameter))
        || (prefix(&["DO"])
            && tokens
                .get(1)
                .is_some_and(|t| t.kind == Kind::Symbol && t.text == "{"))
    {
        Some("procedural scripting is deferred to V3")
    } else {
        None
    };
    match feature {
        Some(message) => Err(Error::Unsupported(message.into())),
        None => Ok(()),
    }
}
