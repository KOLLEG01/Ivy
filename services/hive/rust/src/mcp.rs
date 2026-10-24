//! Direct MCP descriptors and provider/variant selection over retained bindings.
use crate::mcp_schema::McpSchemaBundle;
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    core_schema::validate_input,
    hash::hash_json,
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
    schema::SchemaValidators,
};
use std::{collections::HashMap, sync::OnceLock};

/// Reuses Discovery's language primitives and adds ECMAScript String.toUpperCase.
pub trait Runtime: crate::discovery::Runtime {
    fn uppercase(&self, value: &JsString) -> IvyResult<JsString>;
}

pub fn surface_bindings(bindings: &[IvyJson], surface: &JsString) -> IvyResult<Vec<IvyJson>> {
    fn mcp(b: &IvyJson) -> Option<&IvyJson> {
        definition(b)
            .ok()
            .and_then(|d| d.get("discovery"))
            .and_then(|d| d.get("mcp"))
    }
    let public = bindings
        .iter()
        .filter_map(|b| {
            mcp(b)
                .filter(|m| is_text(m.get("surface"), "ivy"))
                .and_then(|m| m.get("name"))
                .and_then(IvyJson::as_js_string)
                .cloned()
        })
        .collect::<Vec<_>>();
    let mut result = vec![];
    for binding in bindings {
        let descriptor = mcp(binding);
        let declared = descriptor
            .and_then(|m| m.get("surface"))
            .filter(|v| !v.is_null())
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or_else(|| "ivy_dev".into());
        if &declared != surface {
            continue;
        }
        if surface == &JsString::from("ivy_dev") {
            let name = descriptor
                .and_then(|d| d.get("name"))
                .and_then(IvyJson::as_js_string)
                .ok_or_else(IvyError::internal)?;
            if public.contains(name) {
                continue;
            }
        }
        result.push(binding.clone());
    }
    Ok(result)
}

pub fn success(result: &IvyJson, structured: bool) -> IvyResult<IvyJson> {
    if !structured {
        return Ok(IvyJson::object([("content", IvyJson::Array(vec![]))]));
    }
    let data = result.get("data").and_then(IvyJson::as_js_string);
    let png = is_text(result.get("mimeType"), "image/png")
        && is_text(result.get("encoding"), "base64")
        && data.is_some_and(|d| {
            if d.0.len() > 12 * 1024 * 1024
                || !d
                    .0
                    .starts_with(&"iVBORw0KGgo".encode_utf16().collect::<Vec<_>>())
            {
                return false;
            }
            let body = d.0.iter().position(|v| *v == 61).unwrap_or(d.0.len());
            d.0.len() - body <= 2
                && d.0[body..].iter().all(|v| *v == 61)
                && d.0[..body].iter().all(|v| {
                    (65..=90).contains(v)
                        || (97..=122).contains(v)
                        || (48..=57).contains(v)
                        || [43, 47].contains(v)
                })
        });
    let mut metadata = result.clone();
    if png {
        metadata.remove("data");
    }
    let envelope = IvyJson::object([("result", metadata)]);
    let mut content = vec![IvyJson::object([
        ("type", "text".into()),
        ("text", stringify(&envelope, usize::MAX)?.into()),
    ])];
    if png {
        content.push(IvyJson::object([
            ("type", "image".into()),
            ("mimeType", "image/png".into()),
            ("data", IvyJson::String(data.unwrap().clone())),
        ]));
    }
    Ok(IvyJson::object([
        ("content", IvyJson::Array(content)),
        ("structuredContent", envelope),
    ]))
}
fn metadata() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| parse(include_str!("mcp_metadata.json"), usize::MAX).expect("MCP metadata"))
}
/// Order public discovery without changing provider selection or hiding extensions.
pub fn sort_common_tools(tools: &mut [IvyJson]) {
    let common = metadata().get("commonTools").and_then(IvyJson::as_array).unwrap_or(&[]);
    let secondary = metadata().get("secondaryTools").and_then(IvyJson::as_array).unwrap_or(&[]);
    let rank = |tool: &IvyJson| {
        let name = tool.get("name");
        common.iter().position(|v| Some(v) == name).unwrap_or_else(|| secondary.iter().position(|v| Some(v) == name).map(|n| common.len()+1+n).unwrap_or(common.len()))
    };
    tools.sort_by(|a,b| rank(a).cmp(&rank(b)).then_with(|| a.get("name").and_then(IvyJson::as_js_string).cmp(&b.get("name").and_then(IvyJson::as_js_string))));
}
fn empty() -> IvyJson {
    IvyJson::object(std::iter::empty::<(&str, IvyJson)>())
}
fn field<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a IvyJson> {
    v.get(k).ok_or_else(IvyError::internal)
}
fn string<'a>(v: &'a IvyJson, k: &str) -> IvyResult<&'a JsString> {
    field(v, k)?.as_js_string().ok_or_else(IvyError::internal)
}
fn is_text(v: Option<&IvyJson>, s: &str) -> bool {
    v.and_then(IvyJson::as_js_string) == Some(&s.into())
}
fn require(ok: bool, code: &str, message: &str) -> IvyResult<()> {
    if ok {
        Ok(())
    } else {
        Err(IvyError::new(code, message))
    }
}
fn definition(v: &IvyJson) -> IvyResult<&IvyJson> {
    field(v, "definition")
}
fn selector(binding: &IvyJson) -> IvyResult<Option<&'static IvyJson>> {
    Ok(metadata()
        .get("variantSelectors")
        .unwrap()
        .get_key(string(binding, "qualifiedName")?))
}
fn annotations(binding: &IvyJson) -> Option<&IvyJson> {
    binding.get("definition").and_then(|v| v.get("annotations"))
}
fn read_only(binding: &IvyJson) -> bool {
    annotations(binding).and_then(|v| v.get("readOnlyHint")) == Some(&IvyJson::Bool(true))
}
fn outer_operation(binding: &IvyJson) -> IvyResult<bool> {
    Ok(!read_only(binding)
        && definition(binding)?
            .get("inputSchema")
            .and_then(|v| v.get("properties"))
            .and_then(|v| v.get("operationId"))
            .is_none())
}
fn cat(values: &[&JsString]) -> JsString {
    JsString(values.iter().flat_map(|v| v.0.iter().copied()).collect())
}
fn join(values: &[JsString], separator: &str) -> JsString {
    let mut result = vec![];
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            result.extend(separator.encode_utf16());
        }
        result.extend_from_slice(&v.0);
    }
    JsString(result)
}
fn distinct(values: impl IntoIterator<Item = JsString>) -> Vec<JsString> {
    let mut result = vec![];
    for v in values {
        if !result.contains(&v) {
            result.push(v);
        }
    }
    result
}
fn empty_object(schema: &IvyJson) -> bool {
    is_text(schema.get("type"), "object")
        && schema.get("additionalProperties") == Some(&IvyJson::Bool(false))
        && ["properties", "patternProperties"].iter().all(|k| {
            schema
                .get(k)
                .and_then(IvyJson::as_object)
                .is_none_or(|v| v.is_empty())
        })
        && schema
            .get("required")
            .and_then(IvyJson::as_array)
            .is_none_or(|v| v.is_empty())
        && schema.as_object().is_some_and(|values| {
            values.iter().all(|(k, _)| {
                [
                    "type",
                    "additionalProperties",
                    "properties",
                    "patternProperties",
                    "required",
                    "$schema",
                    "$id",
                    "$defs",
                    "title",
                    "description",
                ]
                .iter()
                .any(|s| k == &JsString::from(*s))
            })
        })
}
pub fn has_structured_output(bindings: &[IvyJson]) -> IvyResult<bool> {
    for binding in bindings {
        let schema = field(definition(binding)?, "outputSchema")?;
        if matches!(
            schema,
            IvyJson::Bool(_) | IvyJson::String(_) | IvyJson::Number(_)
        ) {
            return Ok(true);
        }
        if schema.is_null() {
            return Err(IvyError::internal());
        }
        if !is_text(schema.get("type"), "null")
            && schema.get("const") != Some(&IvyJson::Null)
            && !empty_object(schema)
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn pointer_key(v: &[u16]) -> JsString {
    let mut out = vec![];
    let mut i = 0;
    while i < v.len() {
        if v[i] == 126 && v.get(i + 1).is_some_and(|v| [48, 49].contains(v)) {
            out.push(if v[i + 1] == 49 { 47 } else { 126 });
            i += 2;
        } else {
            out.push(v[i]);
            i += 1;
        }
    }
    JsString(out)
}
fn visible_root(schema: &IvyJson) -> IvyJson {
    let Some(reference) = schema.get("$ref").and_then(IvyJson::as_js_string) else {
        return schema.clone();
    };
    let prefix: Vec<_> = "#/$defs/".encode_utf16().collect();
    let Some(name) = reference
        .0
        .strip_prefix(prefix.as_slice())
        .filter(|v| !v.is_empty() && !v.contains(&47))
    else {
        return schema.clone();
    };
    let Some(target) = schema
        .get("$defs")
        .and_then(|v| v.get_key(&pointer_key(name)))
        .filter(|v| {
            is_text(v.get("type"), "object")
                && matches!(
                    v.get("properties"),
                    Some(IvyJson::Object(_) | IvyJson::Array(_))
                )
        })
    else {
        return schema.clone();
    };
    let mut result = schema.clone();
    result.remove("$ref");
    result.insert("type", "object".into());
    result.insert("properties", target.get("properties").unwrap().clone());
    if let Some(required) = target.get("required").filter(|v| v.as_array().is_some()) {
        result.insert("required", required.clone());
    }
    let mut all = schema
        .get("allOf")
        .and_then(IvyJson::as_array)
        .unwrap_or(&[])
        .to_vec();
    all.push(IvyJson::object([(
        "$ref",
        IvyJson::String(reference.clone()),
    )]));
    result.insert("allOf", IvyJson::Array(all));
    result
}
fn service_input(
    bindings: &[&IvyJson],
    bundle: &mut McpSchemaBundle,
    path: &str,
) -> IvyResult<IvyJson> {
    let first = bindings[0];
    let select = selector(first)?;
    let outer = outer_operation(first)?;
    let mut nodes = vec![];
    let mut descriptions = vec![];
    for b in bindings {
        let provider = field(b, "provider")?;
        let id = string(provider, "serviceNodeId")?;
        nodes.push(IvyJson::String(id.clone()));
        descriptions.push(cat(&[
            id,
            &": host ".into(),
            string(provider, "hostId")?,
            &"; ".into(),
            &if provider.get("available").and_then(IvyJson::as_bool) == Some(true) {
                "available"
            } else {
                "unavailable, retain this owner for existing work"
            }
            .into(),
            &".".into(),
        ]));
    }
    let mut properties = IvyJson::object([
        (
            "serviceNodeId",
            IvyJson::object([
                ("type", "string".into()),
                ("enum", IvyJson::Array(nodes)),
                ("description", IvyJson::String(join(&descriptions, " "))),
            ]),
        ),
        (
            "input",
            bundle.embed(
                &visible_root(field(definition(first)?, "inputSchema")?),
                &format!("{path}/properties/input").as_str().into(),
            )?,
        ),
    ]);
    let mut required = vec![IvyJson::from("serviceNodeId"), "input".into()];
    if let Some(select) = select {
        let property = string(select, "property")?;
        properties.insert(
            property.clone(),
            IvyJson::object([("const", field(select, "value")?.clone())]),
        );
        required.push(IvyJson::String(property.clone()));
    }
    if outer {
        let description = if is_text(first.get("serviceName"), "agent-manager") {
            "Caller-generated <runtimeEpoch>:<issuedAtUnixMs>:<nonce>. Read runtimeEpoch from hive_status; use current Unix milliseconds and a unique 1-128 character nonce without a colon. Completed unreferenced outcomes use a bounded FIFO ring without a guaranteed duration; live and unresolved operations remain retained. Evicted identities expire. New IDs must use the current Hive epoch and be at most 60 seconds in the future. Retain this exact ID and check agent_manager_read after an uncertain result."
        } else {
            "Stable identity for this action. Retain it and reconcile the original outcome after a lost reply; follow the owning service's ID format."
        };
        properties.insert(
            "operationId",
            IvyJson::object([
                ("type", "string".into()),
                ("minLength", IvyJson::Number(1.0)),
                ("maxLength", IvyJson::Number(256.0)),
                ("description", description.into()),
            ]),
        );
        required.push("operationId".into());
    }
    Ok(IvyJson::object([
        ("type", "object".into()),
        ("properties", properties),
        ("required", IvyJson::Array(required)),
        ("additionalProperties", false.into()),
    ]))
}
fn core_input(binding: &IvyJson, bundle: &mut McpSchemaBundle, path: &str) -> IvyResult<IvyJson> {
    let mut schema = bundle.embed(
        &visible_root(field(definition(binding)?, "inputSchema")?),
        &path.into(),
    )?;
    let Some(select) = selector(binding)? else {
        return Ok(if empty_object(&schema) {
            IvyJson::object([
                ("type", "object".into()),
                ("additionalProperties", false.into()),
            ])
        } else {
            schema
        });
    };
    require(
        is_text(schema.get("type"), "object")
            && schema
                .get("properties")
                .is_some_and(|v| v.as_object().is_some()),
        "registry_invalid",
        "A selected core MCP variant needs an object input schema.",
    )?;
    let mut properties = field(&schema, "properties")?.clone();
    let key = string(select, "property")?;
    properties.insert(
        key.clone(),
        IvyJson::object([("const", field(select, "value")?.clone())]),
    );
    schema.insert("properties", properties);
    let mut required = schema
        .get("required")
        .and_then(IvyJson::as_array)
        .unwrap_or(&[])
        .to_vec();
    let value = IvyJson::String(key.clone());
    if !required.contains(&value) {
        required.push(value);
    }
    schema.insert("required", IvyJson::Array(required));
    Ok(schema)
}
fn alternatives(branches: Vec<IvyJson>) -> IvyResult<IvyJson> {
    let keys = distinct(branches.iter().flat_map(|b| {
        b.get("properties")
            .and_then(IvyJson::as_object)
            .unwrap_or(&[])
            .iter()
            .map(|(k, _)| k.clone())
    }));
    let mut properties = empty();
    for key in keys {
        let mut choices: Vec<(String, IvyJson)> = vec![];
        for branch in &branches {
            if let Some(value) = branch.get("properties").and_then(|v| v.get_key(&key)) {
                let hash = hash_json(value)?;
                if let Some((_, v)) = choices.iter_mut().find(|(h, _)| h == &hash) {
                    *v = value.clone();
                } else {
                    choices.push((hash, value.clone()));
                }
            }
        }
        let choices: Vec<_> = choices.into_iter().map(|(_, v)| v).collect();
        properties.insert(
            key,
            if choices.len() == 1 {
                choices[0].clone()
            } else {
                IvyJson::object([("anyOf", IvyJson::Array(choices))])
            },
        );
    }
    Ok(IvyJson::object([
        ("type", "object".into()),
        ("properties", properties),
        (
            "allOf",
            IvyJson::Array(vec![IvyJson::object([("oneOf", IvyJson::Array(branches))])]),
        ),
    ]))
}
fn presentation(service: &JsString, name: &JsString) -> Option<&'static IvyJson> {
    metadata()
        .get("presentations")?
        .get_key(service)?
        .get_key(name)
}
fn fallback_title(name: &JsString, runtime: &dyn Runtime) -> IvyResult<JsString> {
    let mut value = name.clone();
    for c in &mut value.0 {
        if *c == 95 {
            *c = 32;
        }
    }
    let first = runtime.uppercase(&JsString(value.0.first().copied().into_iter().collect()))?;
    Ok(cat(&[
        &first,
        &JsString(value.0.get(1..).unwrap_or(&[]).to_vec()),
    ]))
}
pub fn direct_descriptors(bindings: &[IvyJson], runtime: &dyn Runtime) -> IvyResult<Vec<IvyJson>> {
    let mut groups: Vec<(JsString, Vec<&IvyJson>)> = vec![];
    let mut positions = HashMap::new();
    for binding in bindings {
        let name = string(
            field(field(definition(binding)?, "discovery")?, "mcp")?,
            "name",
        )?
        .clone();
        let i = *positions.entry(name.clone()).or_insert_with(|| {
            groups.push((name, vec![]));
            groups.len() - 1
        });
        groups[i].1.push(binding);
    }
    let mut result = vec![];
    for (name, group) in groups {
        let first = group[0];
        let core = field(first, "provider")?.is_null();
        let service = string(first, "serviceName")?;
        require(
            group.iter().all(|v| {
                v.get("serviceName") == first.get("serviceName")
                    && v.get("provider").is_some_and(|v| v.is_null() == core)
            }),
            "registry_invalid",
            "Conflicting MCP tool owners.",
        )?;
        let mut variants: Vec<(String, Vec<&IvyJson>)> = vec![];
        let mut positions = HashMap::new();
        for binding in &group {
            let key = format!(
                "{}:{}:{}",
                hash_json(field(definition(binding)?, "inputSchema")?)?,
                outer_operation(binding)?,
                hash_json(selector(binding)?.unwrap_or(&IvyJson::Null))?
            );
            let i = *positions.entry(key.clone()).or_insert_with(|| {
                variants.push((key, vec![]));
                variants.len() - 1
            });
            variants[i].1.push(*binding);
        }
        let mut input_bundle = McpSchemaBundle::new();
        let mut inputs = vec![];
        for (i, (_, bindings)) in variants.iter().enumerate() {
            let path = if variants.len() == 1 {
                String::new()
            } else {
                format!("/allOf/0/oneOf/{i}")
            };
            inputs.push(if core {
                core_input(bindings[0], &mut input_bundle, &path)?
            } else {
                service_input(bindings, &mut input_bundle, &path)?
            });
        }
        let input = if inputs.len() == 1 {
            inputs.remove(0)
        } else {
            alternatives(inputs)?
        };
        require(
            matches!(input, IvyJson::Object(_) | IvyJson::Array(_)) || input.is_null(),
            "registry_invalid",
            "Direct MCP tools need an object input schema.",
        )?;
        let input = input_bundle.finish(input);
        let mut outputs: Vec<(String, IvyJson)> = vec![];
        for binding in &group {
            let value = field(definition(binding)?, "outputSchema")?;
            let hash = hash_json(value)?;
            if let Some((_, prior)) = outputs.iter_mut().find(|(h, _)| h == &hash) {
                *prior = value.clone();
            } else {
                outputs.push((hash, value.clone()));
            }
        }
        let mut output_bundle = McpSchemaBundle::new();
        let mut output_values = vec![];
        for (i, (_, output)) in outputs.iter().enumerate() {
            let path = if outputs.len() == 1 {
                "/properties/result".to_owned()
            } else {
                format!("/properties/result/anyOf/{i}")
            };
            output_values.push(output_bundle.embed(&visible_root(output), &path.as_str().into())?);
        }
        let result_schema = if output_values.len() == 1 {
            output_values.remove(0)
        } else {
            IvyJson::object([("anyOf", IvyJson::Array(output_values))])
        };
        let present = presentation(service, &name);
        let titles = distinct(group.iter().filter_map(|b| {
            annotations(b)
                .and_then(|a| a.get("title"))
                .and_then(IvyJson::as_js_string)
                .filter(|v| !v.0.is_empty())
                .cloned()
        }));
        let title = if titles.len() == 1 {
            titles[0].clone()
        } else if let Some(v) = present
            .and_then(|v| v.get("title"))
            .and_then(IvyJson::as_js_string)
        {
            v.clone()
        } else {
            fallback_title(&name, runtime)?
        };
        let mut descriptions = distinct(
            group
                .iter()
                .map(|b| {
                    definition(b).and_then(|d| {
                        d.get("discovery")
                            .and_then(|v| v.get("summary"))
                            .filter(|v| !v.is_null())
                            .or_else(|| {
                                present
                                    .and_then(|v| v.get("description"))
                                    .filter(|v| !v.is_null())
                            })
                            .or_else(|| d.get("description"))
                            .and_then(IvyJson::as_js_string)
                            .cloned()
                            .ok_or_else(IvyError::internal)
                    })
                })
                .collect::<IvyResult<Vec<_>>>()?,
        );
        for binding in &group {
            if let Some(select) = selector(binding)?.filter(|_| present.is_none()) {
                descriptions.push(cat(&[
                    &"Set ".into(),
                    string(select, "property")?,
                    &"=".into(),
                    &stringify(field(select, "value")?, MANAGEMENT_FRAME_BYTES)?
                        .as_str()
                        .into(),
                    &" for ".into(),
                    string(binding, "qualifiedName")?,
                    &".".into(),
                ]));
            }
        }
        let mut descriptor = IvyJson::object([
            ("name", IvyJson::String(name)),
            (
                "_meta",
                IvyJson::object([("ivy/serviceName", IvyJson::String(service.clone()))]),
            ),
            ("title", IvyJson::String(title)),
            ("description", IvyJson::String(join(&descriptions, "\n\n"))),
            ("inputSchema", input),
        ]);
        if has_structured_output(&group.iter().map(|v| (*v).clone()).collect::<Vec<_>>())? {
            descriptor.insert(
                "outputSchema",
                output_bundle.finish(IvyJson::object([
                    ("type", "object".into()),
                    ("properties", IvyJson::object([("result", result_schema)])),
                    ("required", IvyJson::Array(vec!["result".into()])),
                    ("additionalProperties", false.into()),
                ])),
            );
        }
        let hint = |binding: &IvyJson, key: &str, presentation_default: bool, default: bool| {
            annotations(binding)
                .and_then(|v| v.get(key))
                .and_then(IvyJson::as_bool)
                .or_else(|| {
                    if presentation_default {
                        present.and_then(|v| v.get(key)).and_then(IvyJson::as_bool)
                    } else {
                        None
                    }
                })
                .unwrap_or(default)
        };
        descriptor.insert(
            "annotations",
            IvyJson::object([
                ("readOnlyHint", group.iter().all(|b| read_only(b)).into()),
                (
                    "destructiveHint",
                    group
                        .iter()
                        .any(|b| hint(b, "destructiveHint", true, !read_only(b)))
                        .into(),
                ),
                (
                    "idempotentHint",
                    group
                        .iter()
                        .all(|b| hint(b, "idempotentHint", false, read_only(b)))
                        .into(),
                ),
                (
                    "openWorldHint",
                    group
                        .iter()
                        .any(|b| {
                            hint(
                                b,
                                "openWorldHint",
                                true,
                                if present.is_some() { false } else { !core },
                            )
                        })
                        .into(),
                ),
            ]),
        );
        result.push(descriptor);
    }
    Ok(result)
}
fn binding_input(binding: &IvyJson, args: &IvyJson) -> IvyResult<IvyJson> {
    if binding
        .get("provider")
        .is_some_and(|v| v.as_object().is_some())
    {
        return field(args, "input").cloned();
    }
    let Some(select) = selector(binding)? else {
        return Ok(args.clone());
    };
    let key = string(select, "property")?;
    Ok(IvyJson::object(
        args.as_object()
            .ok_or_else(IvyError::internal)?
            .iter()
            .filter(|(k, _)| k != key)
            .map(|(k, v)| (k.clone(), v.clone())),
    ))
}
pub fn direct_call(
    bindings: &[IvyJson],
    args: &IvyJson,
    runtime: &dyn Runtime,
) -> IvyResult<IvyJson> {
    static VALIDATORS: OnceLock<SchemaValidators> = OnceLock::new();
    let validators = VALIDATORS.get_or_init(SchemaValidators::new);
    let mut matches = vec![];
    for binding in bindings {
        if !binding.get("provider").is_some_and(IvyJson::is_null)
            && binding.get("provider").and_then(|v| v.get("serviceNodeId"))
                != args.get("serviceNodeId")
        {
            continue;
        }
        let matched = (|| -> IvyResult<()> {
            if binding
                .get("provider")
                .is_some_and(|v| v.as_object().is_some())
                || selector(binding)?.is_some()
            {
                let descriptors = direct_descriptors(std::slice::from_ref(binding), runtime)?;
                validators.validate(
                    field(&descriptors[0], "inputSchema")?,
                    args,
                    MANAGEMENT_FRAME_BYTES,
                )?;
            }
            if !binding
                .get("provider")
                .is_some_and(|v| v.as_object().is_some())
            {
                let method = string(definition(binding)?, "name")?.to_utf8()?;
                validate_input(&method, &binding_input(binding, args)?)?;
            }
            Ok(())
        })();
        if matched.is_ok() {
            matches.push(binding);
        }
    }
    require(
        matches.len() == 1,
        "invalid_arguments",
        "Select one valid view or action variant and the exact owning serviceNodeId from this tool’s schema.",
    )?;
    let binding = matches[0];
    let input = binding_input(binding, args)?;
    let mut result = IvyJson::object([
        ("serviceName", field(binding, "serviceName")?.clone()),
        ("qualifiedName", field(binding, "qualifiedName")?.clone()),
        (
            "expectedDefinitionHash",
            field(binding, "definitionHash")?.clone(),
        ),
    ]);
    if binding
        .get("provider")
        .is_some_and(|v| v.as_object().is_some())
    {
        result.insert(
            "serviceNodeId",
            field(field(binding, "provider")?, "serviceNodeId")?.clone(),
        );
        if let Some(id) = args
            .get("operationId")
            .filter(|v| !v.is_null())
            .or_else(|| input.get("operationId"))
            .and_then(IvyJson::as_js_string)
        {
            result.insert("operationId", IvyJson::String(id.clone()));
        }
    }
    result.insert("arguments", input);
    Ok(result)
}
