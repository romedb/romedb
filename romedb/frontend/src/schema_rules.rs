use crate::{Collection, Document, Error, FieldType, Result, Value};
use std::collections::BTreeMap;

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    flexible: bool,
}

pub(crate) fn validate_definition(collection: &Collection) -> Result<()> {
    if !collection.strict {
        return Ok(());
    }
    if collection.fields.len() > 1024 || collection.fields.iter().any(|field| field.path.len() > 64)
    {
        return Err(Error::Limit(
            "strict schema exceeds 1024 fields or 64 path steps".into(),
        ));
    }
    for field in &collection.fields {
        if !matches!(field.kind, FieldType::Object)
            && collection.fields.iter().any(|other| {
                other.path.len() > field.path.len() && other.path.starts_with(&field.path)
            })
        {
            return Err(Error::Validation(format!(
                "strict schema parent {} must be an object",
                field.path.join(".")
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate(collection: &Collection, document: &Document) -> Result<()> {
    if !collection.strict {
        return Ok(());
    }
    let mut root = Node::default();
    root.children.insert("id".into(), Node::default());
    if collection.edge.is_some() {
        root.children.insert("source".into(), Node::default());
        root.children.insert("target".into(), Node::default());
    }
    for field in &collection.fields {
        let mut node = &mut root;
        for key in &field.path {
            node = node.children.entry(key.clone()).or_default();
        }
    }
    for policy in collection
        .field_policies
        .iter()
        .filter(|policy| policy.options.flexible)
    {
        let mut node = &mut root;
        for key in &policy.path {
            node = node.children.get_mut(key).expect("validated field policy");
        }
        node.flexible = true;
    }
    struct Walker {
        work: usize,
    }
    impl Walker {
        fn step(&mut self) -> Result<()> {
            self.work += 1;
            if self.work > 1_000_000 {
                return Err(Error::Limit(
                    "strict schema validation exceeds 1000000 steps".into(),
                ));
            }
            Ok(())
        }
        fn value(
            &mut self,
            value: &Value,
            node: &Node,
            flexible: bool,
            path: &mut Vec<String>,
        ) -> Result<()> {
            match value {
                Value::Object(object) => self.object(object, node, flexible, path),
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate() {
                        self.step()?;
                        path.push(format!("[{index}]"));
                        self.value(value, node, flexible, path)?;
                        path.pop();
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        fn object(
            &mut self,
            document: &Document,
            node: &Node,
            flexible: bool,
            path: &mut Vec<String>,
        ) -> Result<()> {
            let flexible = flexible || node.flexible;
            for (key, value) in document {
                self.step()?;
                path.push(key.clone());
                if let Some(child) = node.children.get(key) {
                    self.value(value, child, flexible || child.flexible, path)?;
                } else if !flexible {
                    return Err(Error::Validation(format!(
                        "undeclared field {} in strict collection",
                        path.join(".")
                    )));
                }
                path.pop();
            }
            Ok(())
        }
    }
    Walker { work: 0 }.object(document, &root, false, &mut Vec::new())
}

impl crate::Connection {
    pub fn define_schema(&self, table: &str, strict: bool) -> Result<()> {
        self.atomic(|| {
            let mut collection = self.catalog(table)?;
            collection.strict = strict;
            if strict {
                collection.version = collection.version.max(5);
            }
            crate::field_rules::validate_catalog(&collection)?;
            for document in self.documents(&collection)? {
                self.validate_candidate(&collection, &document)?;
                self.validate_computed_fields(&collection, &document)?;
            }
            self.save_catalog(&collection)
        })
    }
}
