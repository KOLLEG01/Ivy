//! Version-bound native discovery using the shared English UTF-16 casing.
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    agent_schema::validate_agent,
    hash::{hash_json, tool_definition_hash},
    json::try_js_string,
};

fn whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn search_text(definition: &IvyJson) -> IvyResult<JsString> {
    let discovery = definition.get("discovery");
    let values = [
        definition.get("name"),
        definition.get("description"),
        discovery.and_then(|value| value.get("summary")),
        discovery.and_then(|value| value.get("group")),
    ];
    let mut joined = JsString::default();
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            joined.0.push(32);
        }
        if let Some(value) = value.filter(|value| !value.is_null()) {
            joined.0.extend(try_js_string(value)?.0);
        }
    }
    Ok(joined)
}

pub fn discover_native_tools(
    catalog: &IvyJson,
    definitions: &[IvyJson],
    input: &IvyJson,
    platform: &str,
) -> IvyResult<IvyJson> {
    validate_agent("NativeDiscoveryInput", input)?;
    let method = input.get("method").and_then(IvyJson::as_js_string);
    let query = input.get("query").and_then(IvyJson::as_js_string);
    if method.is_some() && query.is_some() {
        return Err(IvyError::new(
            "invalid_arguments",
            "Choose capability keywords or one exact native method.",
        ));
    }
    let lowered = query
        .map(JsString::to_english_lowercase)
        .unwrap_or_default();
    let words: Vec<_> = lowered
        .0
        .split(|unit| whitespace(*unit))
        .filter(|word| !word.is_empty())
        .collect();
    let mut selected = Vec::new();
    for definition in definitions {
        let include = if let Some(method) = method {
            definition
                .get("nativeMethod")
                .and_then(IvyJson::as_js_string)
                == Some(method)
        } else if words.is_empty() {
            true
        } else {
            let text = search_text(definition)?.to_english_lowercase();
            words
                .iter()
                .all(|word| text.0.windows(word.len()).any(|window| window == *word))
        };
        if include {
            selected.push(definition);
        }
    }
    if method.is_some() && selected.len() != 1 {
        return Err(IvyError::new(
            "native_method_unsupported",
            "The method is absent from this host’s installed native catalog.",
        ));
    }
    let mut items = Vec::with_capacity(selected.len());
    for definition in selected {
        let summary = definition
            .get("discovery")
            .and_then(|value| value.get("summary"))
            .filter(|value| !value.is_null())
            .or_else(|| definition.get("description"));
        let mut item = IvyJson::object([
            ("description", summary.cloned().unwrap_or(IvyJson::Null)),
            (
                "readOnlyHint",
                (definition
                    .get("annotations")
                    .and_then(|value| value.get("readOnlyHint"))
                    == Some(&IvyJson::Bool(true)))
                .into(),
            ),
            (
                "expectedDefinitionHash",
                tool_definition_hash(definition)?.into(),
            ),
        ]);
        if let Some(native_method) = definition.get("nativeMethod") {
            item.insert("method", native_method.clone());
        }
        if method.is_some() {
            for key in ["inputSchema", "outputSchema"] {
                if let Some(schema) = definition.get(key) {
                    item.insert(key, schema.clone());
                }
            }
            if platform == "win32" && definition.get("nativeMethod") == Some(&"command/exec".into())
            {
                item.insert("runtimeConstraints", IvyJson::Array(vec![IvyJson::object([
                    ("condition", "Windows sandbox is active".into()),
                    ("unsupportedParameters", IvyJson::Array(vec!["outputBytesCap".into()])),
                    ("guidance", "Omit a custom outputBytesCap when executing in the Windows sandbox. The native method schema remains unchanged.".into()),
                ])]));
            }
        }
        items.push(item);
    }
    Ok(IvyJson::object([
        ("nativeVersion", catalog.required("version")?.clone()),
        ("catalogHash", hash_json(catalog)?.into()),
        ("items", IvyJson::Array(items)),
    ]))
}
