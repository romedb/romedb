use crate::{Document, Error, Result, Value};
use romeql_parser::MutationMode;

pub(crate) fn apply(mode: &MutationMode, before: &Document, value: Value) -> Result<Document> {
    if matches!(mode, MutationMode::Patch) {
        return crate::patch::apply(before, value);
    }
    let Value::Object(mut value) = value else {
        return Err(Error::Validation(
            "CONTENT and MERGE require an object".into(),
        ));
    };
    let Some(Value::Record(id)) = before.get("id") else {
        return Err(Error::Storage("mutation source has no typed id".into()));
    };
    if let Some(provided) = value.remove("id") {
        let Value::Record(provided) = provided else {
            return Err(Error::Validation("id is immutable".into()));
        };
        if crate::normalized_id(&provided, &id.table)? != *id {
            return Err(Error::Validation("id is immutable".into()));
        }
    }
    let mut document = match mode {
        MutationMode::Content => value,
        MutationMode::Merge => {
            let mut document = before.clone();
            merge(&mut document, value);
            document
        }
        MutationMode::Patch => unreachable!(),
    };
    document.insert("id".into(), Value::Record(id.clone()));
    crate::value::validate_document_value(&document)?;
    Ok(document)
}

fn merge(document: &mut Document, incoming: Document) {
    for (key, value) in incoming {
        match (document.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(incoming)) => merge(existing, incoming),
            (_, value) => {
                document.insert(key, value);
            }
        }
    }
}
