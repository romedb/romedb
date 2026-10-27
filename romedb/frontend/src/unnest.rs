use crate::{Error, Result, Value};
use std::cell::RefCell;

thread_local! {
    static BUDGET: RefCell<Option<Budget>> = const { RefCell::new(None) };
}

struct Budget {
    elements: usize,
    bytes: crate::links::FetchBudget,
}

pub(crate) struct Scope(Option<Budget>);

impl Scope {
    pub(crate) fn enter() -> Self {
        Self(BUDGET.with(|slot| {
            slot.replace(Some(Budget {
                elements: 0,
                bytes: crate::links::FetchBudget {
                    used: 0,
                    limit: 64 * 1024 * 1024,
                },
            }))
        }))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        BUDGET.with(|slot| slot.replace(self.0.take()));
    }
}

pub(crate) fn json(input: &Value) -> Result<String> {
    let values = match input {
        Value::Array(values) => values.as_slice(),
        Value::Null => &[],
        _ => {
            return Err(Error::Validation(
                "array::unnest expects an array or null".into(),
            ))
        }
    };
    if values.len() > 100_000 {
        return Err(Error::Limit(
            "array::unnest input exceeds 100000 elements".into(),
        ));
    }
    BUDGET.with(|slot| {
        let mut slot = slot.borrow_mut();
        let budget = slot
            .as_mut()
            .ok_or_else(|| Error::Storage("unnest execution scope missing".into()))?;
        budget.elements = budget.elements.saturating_add(values.len());
        if budget.elements > 1_000_000 {
            return Err(Error::Limit(
                "array::unnest expansion exceeds 1000000 elements".into(),
            ));
        }
        budget.bytes.charge(input)?;
        let mut output = String::from("[");
        for (position, value) in values.iter().enumerate() {
            let encoded =
                String::from_utf8(value.encode()?).map_err(|e| Error::Storage(e.to_string()))?;
            let string = Value::String(encoded);
            budget.bytes.charge(&string)?;
            if position != 0 {
                output.push(',');
            }
            let Value::String(string) = string else {
                unreachable!()
            };
            output.push_str(&serde_json::to_string(&string)?);
        }
        output.push(']');
        Ok(output)
    })
}
