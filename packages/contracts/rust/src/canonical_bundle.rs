//! Bundling for checked-in canonical documents, separate from submitted schemas.
use crate::{IvyError, IvyJson, IvyResult, JsString, json::parse};
use url::Url;

/// Trusted packaged documents and their stable definition-name prefixes.
pub struct Bundle {
    documents: Vec<(String, IvyJson)>,
    label: String,
}
struct Resolved<'a> {
    value: &'a IvyJson,
    document: usize,
    alias: String,
}
impl Bundle {
    pub(crate) fn new(documents: &[(&'static str, &str)], label: &'static str) -> Self {
        Self {
            documents: documents
                .iter()
                .map(|(prefix, text)| {
                    (
                        (*prefix).to_owned(),
                        parse(text, text.len()).expect("checked-in schema"),
                    )
                })
                .collect(),
            label: label.to_owned(),
        }
    }
    /// Load owned trusted schemas. Prefixes retain the public bundled $defs names.
    /// Submitted registry schemas use schema admission instead.
    pub fn from_documents(documents: Vec<(String, IvyJson)>) -> IvyResult<Self> {
        let mut identities = std::collections::HashSet::new();
        let mut prefixes = std::collections::HashSet::new();
        for (prefix, document) in &documents {
            let id = document
                .get("$id")
                .ok_or_else(IvyError::internal)?
                .as_str()?;
            if prefix.is_empty()
                || !prefix.bytes().all(|c| c.is_ascii_alphanumeric())
                || !prefixes.insert(prefix.clone())
                || !identities.insert(id)
            {
                return Err(IvyError::new(
                    "internal_error",
                    "Invalid trusted schema catalog.",
                ));
            }
        }
        Ok(Self {
            documents,
            label: "canonical".to_owned(),
        })
    }
    /// Bundle a named definition from the supplied canonical document ID.
    pub fn definition(&self, document_id: &str, name: &str) -> IvyResult<IvyJson> {
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Err(IvyError::new(
                "internal_error",
                "Invalid schema definition name.",
            ));
        }
        let document = self
            .documents
            .iter()
            .position(|(_, schema)| {
                schema
                    .get("$id")
                    .is_some_and(|id| id.as_str().ok().as_deref() == Some(document_id))
            })
            .ok_or_else(|| {
                IvyError::new("internal_error", "Unknown canonical schema reference.")
            })?;
        self.bundle(&format!("#/$defs/{name}").into(), None, document)
    }
    fn resolve(&self, reference: &JsString, base: &JsString) -> IvyResult<Resolved<'_>> {
        let base = Url::parse(&base.to_utf8()?).map_err(|_| IvyError::internal())?;
        let url = base
            .join(&reference.to_utf8()?)
            .map_err(|_| IvyError::internal())?;
        let document_id = url.origin().ascii_serialization() + url.path();
        let unknown = || {
            IvyError::new(
                "internal_error",
                format!("Unknown {} schema reference.", self.label),
            )
        };
        let document = self
            .documents
            .iter()
            .position(|(_, schema)| {
                schema.get("$id").and_then(IvyJson::as_js_string)
                    == Some(&JsString::from(document_id.as_str()))
            })
            .ok_or_else(unknown)?;
        let name = url
            .fragment()
            .and_then(|hash| hash.strip_prefix("/$defs/"))
            .filter(|name| !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric()))
            .ok_or_else(unknown)?;
        let (prefix, schema) = &self.documents[document];
        let value = schema
            .get("$defs")
            .and_then(|defs| defs.get(name))
            .ok_or_else(|| {
                IvyError::new(
                    "internal_error",
                    format!("Missing {} schema definition.", self.label),
                )
            })?;
        Ok(Resolved {
            value,
            document,
            alias: format!("{prefix}{name}"),
        })
    }
    fn id(&self, document: usize) -> &JsString {
        self.documents[document]
            .1
            .get("$id")
            .and_then(IvyJson::as_js_string)
            .expect("canonical document ID")
    }
    pub(crate) fn bundle(
        &self,
        reference: &JsString,
        base: Option<&JsString>,
        default: usize,
    ) -> IvyResult<IvyJson> {
        let mut root = self.resolve(reference, base.unwrap_or_else(|| self.id(default)))?;
        while let Some(reference) = root.value.get("$ref").and_then(IvyJson::as_js_string) {
            root = self.resolve(reference, self.id(root.document))?;
        }
        enum Work<'a> {
            Visit(&'a IvyJson, usize),
            Property(&'a JsString, &'a IvyJson, usize),
            SetProperty(JsString),
            Append,
            Literal(IvyJson),
            StoreDefinition(String),
        }
        let mut definitions = IvyJson::Object(Vec::new());
        let mut pending = vec![Work::Visit(root.value, root.document)];
        let mut values = Vec::<IvyJson>::new();
        while let Some(work) = pending.pop() {
            match work {
                Work::Visit(value, document) => match value {
                    IvyJson::Object(pairs) => {
                        values.push(IvyJson::Object(Vec::new()));
                        for (key, value) in pairs.iter().rev() {
                            if key != &JsString::from("$id") && key != &JsString::from("$schema") {
                                pending.push(Work::Property(key, value, document));
                            }
                        }
                    }
                    IvyJson::Array(items) => {
                        values.push(IvyJson::Array(Vec::new()));
                        for item in items.iter().rev() {
                            pending.push(Work::Append);
                            pending.push(Work::Visit(item, document));
                        }
                    }
                    _ => values.push(value.clone()),
                },
                Work::Property(key, value, document) => {
                    pending.push(Work::SetProperty(key.clone()));
                    if key != &JsString::from("$ref") {
                        pending.push(Work::Visit(value, document));
                        continue;
                    }
                    let reference = value.as_js_string().ok_or_else(IvyError::internal)?;
                    let resolved = self.resolve(reference, self.id(document))?;
                    pending.push(Work::Literal(IvyJson::from(format!(
                        "#/$defs/{}",
                        resolved.alias
                    ))));
                    if definitions.get(&resolved.alias).is_none() {
                        // The placeholder both breaks recursive graphs and preserves DFS order.
                        definitions.insert(resolved.alias.as_str(), IvyJson::Bool(true));
                        pending.push(Work::StoreDefinition(resolved.alias));
                        pending.push(Work::Visit(resolved.value, resolved.document));
                    }
                }
                Work::SetProperty(key) => {
                    let value = values.pop().unwrap();
                    values.last_mut().unwrap().insert(key, value);
                }
                Work::Append => {
                    let value = values.pop().unwrap();
                    let IvyJson::Array(items) = values.last_mut().unwrap() else {
                        unreachable!()
                    };
                    items.push(value);
                }
                Work::Literal(value) => values.push(value),
                Work::StoreDefinition(alias) => definitions.insert(alias, values.pop().unwrap()),
            }
        }
        let mut result = values.pop().expect("bundled root");
        result.insert("$defs", definitions);
        Ok(result)
    }
}
