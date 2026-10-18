//! Bundling for checked-in canonical documents, separate from submitted schemas.
use crate::{
    IvyError, IvyJson, IvyResult, JsString,
    json::{js_string, parse},
};
use url::Url;

// The TS canonical catalogs are ordinary objects, including inherited lookups.
pub(crate) fn inherited_property(name: &str) -> bool {
    matches!(
        name,
        "constructor"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "toLocaleString"
            | "toString"
            | "valueOf"
            | "__proto__"
            | "__defineGetter__"
            | "__defineSetter__"
            | "__lookupGetter__"
            | "__lookupSetter__"
    )
}

pub(crate) struct Bundle {
    documents: Vec<(&'static str, IvyJson)>,
    label: &'static str,
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
                    (*prefix, parse(text, text.len()).expect("checked-in schema"))
                })
                .collect(),
            label,
        }
    }
    fn resolve(&self, reference: &JsString, base: &JsString) -> IvyResult<Resolved<'_>> {
        // URL takes USVString in JavaScript; only URL inputs replace lone surrogates.
        // Parse the base even for absolute references, matching new URL(ref, base).
        let base =
            Url::parse(&String::from_utf16_lossy(&base.0)).map_err(|_| IvyError::internal())?;
        let url = base
            .join(&String::from_utf16_lossy(&reference.0))
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
            .or_else(|| {
                // Allowed inherited names resolve to functions with no enumerable fields.
                static EMPTY: IvyJson = IvyJson::Object(Vec::new());
                inherited_property(name).then_some(&EMPTY)
            })
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
                    let resolved = self.resolve(&js_string(value), self.id(document))?;
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
