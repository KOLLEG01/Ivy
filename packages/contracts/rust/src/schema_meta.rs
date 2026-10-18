//! The pinned compiler's meta-schemas, independent of submitted RE2 patterns.
use crate::{IvyError, IvyJson, IvyResult, builtin_schema::SchemaBundle, json::parse};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULT: &str = "https://json-schema.org/draft/2020-12/schema";
static BUNDLE: OnceLock<IvyResult<MetaSchemas>> = OnceLock::new();

struct MetaSchemas {
    bundle: SchemaBundle,
    lookup: Lookup,
}
#[derive(Deserialize)]
struct Lookup {
    roots: HashMap<String, usize>,
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    mode: String,
    reference: Option<String>,
    own: HashMap<String, i32>,
    prototype: Option<usize>,
}
impl Lookup {
    fn property(&self, receiver: usize, key: &str) -> Option<usize> {
        let mut current = receiver;
        loop {
            let node = self.nodes.get(current)?;
            if let Some(index) = node.own.get(key) {
                return match *index {
                    -2 => self.nodes[receiver].prototype,
                    index if index >= 0 => Some(index as usize),
                    _ => None,
                };
            }
            current = node.prototype?;
        }
    }
    fn resolve(&self, reference: &str) -> IvyResult<&Node> {
        let (id, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let id = if id == "http://json-schema.org/schema" {
            DEFAULT
        } else {
            id
        };
        let mut index = *self.roots.get(id).ok_or_else(invalid)?;
        if !fragment.is_empty() {
            for part in fragment.strip_prefix('/').ok_or_else(invalid)?.split('/') {
                let part = decode_fragment(part)?.replace("~1", "/").replace("~0", "~");
                index = self.property(index, &part).ok_or_else(invalid)?;
            }
        }
        self.nodes.get(index).ok_or_else(invalid)
    }
}

fn decode_fragment(text: &str) -> IvyResult<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let high = (input.next().ok_or_else(invalid)? as char)
                .to_digit(16)
                .ok_or_else(invalid)?;
            let low = (input.next().ok_or_else(invalid)? as char)
                .to_digit(16)
                .ok_or_else(invalid)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}

fn invalid() -> IvyError {
    IvyError::new("registry_invalid", "Invalid or unsupported JSON schema.")
}

pub(super) fn validate(schema: &IvyJson) -> IvyResult<()> {
    if matches!(schema, IvyJson::Bool(_)) {
        return Ok(());
    }
    let selected = schema
        .get("$schema")
        .map(IvyJson::as_str)
        .transpose()
        .map_err(|_| invalid())?;
    let selected = selected
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT);
    // AJV strips a trailing empty fragment (with or without its slash).
    let selected = selected
        .strip_suffix("#/")
        .or_else(|| selected.strip_suffix('#'))
        .unwrap_or(selected);
    let bundle = BUNDLE
        .get_or_init(|| {
            let source = include_str!("schema-meta-data.json");
            let data = parse(source, 1024 * 1024)?;
            let schemas = data.get("schemas").ok_or_else(IvyError::internal)?;
            let bundle = SchemaBundle::from_documents(
                schemas
                    .clone()
                    .into_array()
                    .ok_or_else(IvyError::internal)?,
            )?;
            let lookup = serde_json::from_str(source).map_err(|_| IvyError::internal())?;
            Ok(MetaSchemas { bundle, lookup })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let target = bundle.lookup.resolve(selected)?;
    let valid = match target.mode.as_str() {
        "accept" => true,
        "reject" => false,
        "schema" => bundle
            .bundle
            .matches(target.reference.as_deref().ok_or_else(invalid)?, schema)
            .map_err(|_| invalid())?,
        _ => return Err(invalid()),
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
