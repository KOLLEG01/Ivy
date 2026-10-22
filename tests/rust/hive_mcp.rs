use super::*;
fn operation(method: &str, params: Value) -> Value {
    let mut v = action(method, None, params);
    v["op"] = json!("discovery");
    v
}
pub(super) fn binding(
    name: &str,
    qualified: &str,
    input: Value,
    output: Value,
    read: bool,
    node: Option<&str>,
) -> Value {
    let mut definition = json!({"namespace":"fixture","name":qualified.split_once('.').unwrap().1,"interfaceVersion":"1.0.0","description":"Synthetic tool description.","discovery":{"mcp":{"name":name,"surface":"ivy"}},"inputSchema":input,"outputSchema":output,"annotations":{"readOnlyHint":read}});
    if node.is_none() {
        definition["namespace"] = json!("hive");
    }
    json!({"qualifiedName":qualified,"definition":definition,"definitionHash":format!("sha256:{}","a".repeat(64)),"serviceName":if node.is_some(){"sample"}else{"hive"},"provider":node.map(|n|json!({"serviceNodeId":n,"hostId":format!("host-{n}"),"available":true})),"guideMarkdown":"Synthetic guide."})
}
pub(super) fn input(kind: &str) -> Value {
    json!({"type":"object","properties":{"value":{"type":kind}},"required":["value"],"additionalProperties":false})
}
fn descriptors(bindings: Vec<Value>) -> Value {
    operation("directDescriptors", json!({"bindings":bindings}))
}
fn call(bindings: Vec<Value>, args: Value) -> Value {
    operation("directCall", json!({"bindings":bindings,"args":args}))
}
fn bundle(entries: Vec<Value>) -> Value {
    operation("mcpBundle", json!({"entries":entries}))
}
pub(super) fn ready(s: &mut Vec<Value>, id: &str, tools: Vec<Value>) {
    let mut c = catalog("fixture");
    c["namespaces"][0]["tools"] = json!(tools);
    for mut step in [
        connect(id, "sample", &format!("host-{id}"), 1),
        action("sync", Some(id), c),
        heartbeat(id, true, vec![]),
    ] {
        step["requireSuccess"] = json!(true);
        push(s, step);
    }
}
pub(super) fn defined(name: &str, mcp: &str, surface: &str, kind: &str, read: bool) -> Value {
    json!({"namespace":"fixture","name":name,"interfaceVersion":"1.0.0","description":"Synthetic registered tool.","inputSchema":input(kind),"outputSchema":{"type":kind},"annotations":{"readOnlyHint":read},"discovery":{"mcp":{"name":mcp,"surface":surface}}})
}

#[test]
fn mcp_schema_graph_sharing_cycles_pointer_escaping_and_path_identity_match_original() {
    let mut s = setup();
    let base = json!({"type":"object","properties":{"value":{"$ref":"#/$defs/Item"}},"$defs":{"Item":{"type":"string"}}});
    let number = json!({"type":"object","properties":{"value":{"$ref":"#/$defs/Item"}},"$defs":{"Item":{"type":"number"}}});
    let alias = json!({"properties":{"value":{"$ref":"#/$defs/Alias"}},"$defs":{"Alias":{"type":"string"}}});
    let outside = json!({"properties":{"target":{"type":"string"},"value":{"$ref":"#/$defs/Outside"}},"$defs":{"Outside":{"$ref":"#/properties/target"}}});
    let cycle = json!({"$ref":"#/$defs/Node","$defs":{"Node":{"type":"object","properties":{"next":{"anyOf":[{"type":"null"},{"$ref":"#/$defs/Node"}]}}}}});
    let escaped = json!({"$ref":"#/$defs/a~1b~0c","$defs":{"a/b~c":{"type":"boolean"},"__proto__":{"type":"integer"},"2":{"type":"null"}}});
    let index = push(
        &mut s,
        bundle(vec![
            json!({"schema":base,"path":"/a"}),
            json!({"schema":number,"path":"/b"}),
            json!({"schema":alias,"path":"/c"}),
            json!({"schema":outside,"path":"/left"}),
            json!({"schema":outside,"path":"/right"}),
            json!({"schema":cycle,"path":"/cycle"}),
            json!({"schema":escaped,"path":"/escaped"}),
        ]),
    );
    let r = compare(&s);
    success(&r, index);
    let v = value(&r, index);
    assert_eq!(
        v["embeds"][1]["properties"]["value"]["$ref"],
        "#/$defs/Item_2"
    );
    assert_eq!(
        v["embeds"][2]["properties"]["value"]["$ref"],
        "#/$defs/Item"
    );
    assert_eq!(
        v["finished"]["$defs"]["Outside"]["$ref"],
        "#/left/properties/target"
    );
    assert_eq!(
        v["finished"]["$defs"]["Outside_2"]["$ref"],
        "#/right/properties/target"
    );
    assert_eq!(
        v["finished"]["$defs"]["Node"]["properties"]["next"]["anyOf"][1]["$ref"],
        "#/$defs/Node"
    );
    assert_eq!(v["embeds"][6]["$ref"], "#/$defs/a~1b~0c");
    assert_eq!(v["finished"]["$defs"]["__proto__"]["type"], "integer");
}

#[test]
fn mcp_schema_rewrites_only_schema_positions_and_rejects_missing_definition_targets_match_original()
{
    let mut s = setup();
    let node = json!({"$id":"local","$schema":"meta","$ref":"#/properties/root"});
    let mut schema = json!({"$id":"root","$schema":"meta","type":"object","properties":{"root":{"type":"string"}},"default":{"$id":"literal","$ref":"#literal"},"examples":[{"$schema":"literal","$ref":"#literal"}],"const":{"$ref":"#literal"}});
    for key in ["$defs", "patternProperties", "dependentSchemas"] {
        schema[key] = json!({"entry":node});
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        schema[key] = json!([node]);
    }
    for key in [
        "items",
        "additionalProperties",
        "unevaluatedProperties",
        "not",
        "if",
        "then",
        "else",
        "contains",
        "propertyNames",
    ] {
        schema[key] = node.clone();
    }
    let rewritten = push(
        &mut s,
        bundle(vec![
            json!({"schema":schema,"path":"/branch"}),
            json!({"schema":true,"path":"/bool"}),
        ]),
    );
    let missing = push(
        &mut s,
        bundle(vec![
            json!({"schema":{"$defs":{"Bad":{"$ref":"#/$defs/absent"}}},"path":"/bad"}),
        ]),
    );
    let numeric = push(
        &mut s,
        bundle(vec![
            json!({"schema":{"required":["x"],"$defs":{"Entry":{"$ref":"#/required/0"}}},"path":"/array"}),
        ]),
    );
    let r = compare(&s);
    for i in [rewritten, numeric] {
        success(&r, i);
    }
    let v = &value(&r, rewritten)["embeds"][0];
    assert!(v.get("$id").is_none());
    assert_eq!(v["default"]["$id"], "literal");
    assert_eq!(v["examples"][0]["$ref"], "#literal");
    assert_eq!(v["items"]["$ref"], "#/branch/properties/root");
    assert_eq!(value(&r, rewritten)["embeds"][1], true);
    assert_eq!(code(&r, missing), "registry_invalid");
}

#[test]
fn mcp_service_provider_variants_expose_fields_keep_all_constraints_and_share_definitions_match_original()
 {
    let mut s = setup();
    let schema = json!({"$ref":"#/$defs/Input","$defs":{"Input":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false}}});
    let output = json!({"$ref":"#/$defs/Output","$defs":{"Output":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}});
    let mut a = binding(
        "sample_run",
        "fixture.read",
        schema.clone(),
        output.clone(),
        true,
        Some("a"),
    );
    a["provider"]["available"] = json!(false);
    let b = binding(
        "sample_run",
        "fixture.read",
        schema,
        output,
        true,
        Some("b"),
    );
    let numeric = binding(
        "sample_run",
        "fixture.write",
        input("number"),
        json!({"type":"number"}),
        false,
        Some("a"),
    );
    let same = push(&mut s, descriptors(vec![a.clone(), b.clone()]));
    let variants = push(
        &mut s,
        descriptors(vec![a.clone(), b.clone(), numeric.clone()]),
    );
    let read = push(
        &mut s,
        call(
            vec![a.clone(), b.clone(), numeric.clone()],
            json!({"serviceNodeId":"a","input":{"value":"hello"}}),
        ),
    );
    let write = push(
        &mut s,
        call(
            vec![a.clone(), numeric.clone()],
            json!({"serviceNodeId":"a","input":{"value":3},"operationId":"stable"}),
        ),
    );
    let missing_id = push(
        &mut s,
        call(
            vec![numeric],
            json!({"serviceNodeId":"a","input":{"value":3}}),
        ),
    );
    let wrong_owner = push(
        &mut s,
        call(
            vec![a, b],
            json!({"serviceNodeId":"absent","input":{"value":"hello"}}),
        ),
    );
    let r = compare(&s);
    for i in [same, variants, read, write] {
        success(&r, i);
    }
    let v = &value(&r, same)[0];
    assert_eq!(
        v["inputSchema"]["properties"]["serviceNodeId"]["enum"],
        json!(["a", "b"])
    );
    assert!(
        v["inputSchema"]["properties"]["serviceNodeId"]["description"]
            .as_str()
            .unwrap()
            .contains("unavailable, retain this owner")
    );
    assert_eq!(
        v["inputSchema"]["properties"]["input"]["properties"]["value"]["type"],
        "string"
    );
    assert!(v["inputSchema"].get("$defs").is_some());
    assert!(
        value(&r, variants)[0]["inputSchema"]["allOf"][0]["oneOf"]
            .as_array()
            .unwrap()
            .len()
            == 2
    );
    assert_eq!(value(&r, read)["qualifiedName"], "fixture.read");
    assert_eq!(value(&r, write)["operationId"], "stable");
    for i in [missing_id, wrong_owner] {
        assert_eq!(code(&r, i), "invalid_arguments");
    }
}

#[test]
fn mcp_actual_core_catalog_aliases_select_views_and_preserve_canonical_mutation_arguments_match_original()
 {
    let mut s = setup();
    let all = push(&mut s, operation("directDescriptors", json!({})));
    let plain = push(
        &mut s,
        operation(
            "directCall",
            json!({"name":"hive_object_read","args":{"objectId":"00000000-0000-4000-8000-000000000001"}}),
        ),
    );
    let metadata = push(
        &mut s,
        operation(
            "directCall",
            json!({"name":"hive_object_read","args":{"objectId":"00000000-0000-4000-8000-000000000001","view":"metadata"}}),
        ),
    );
    let history = push(
        &mut s,
        operation(
            "directCall",
            json!({"name":"hive_object_read","args":{"objectId":"00000000-0000-4000-8000-000000000001","view":"history"}}),
        ),
    );
    let invalid = push(
        &mut s,
        operation(
            "directCall",
            json!({"name":"hive_object_read","args":{"objectId":"bad","view":"missing"}}),
        ),
    );
    let status = push(
        &mut s,
        operation("directCall", json!({"name":"hive_status","args":{}})),
    );
    let outer = push(
        &mut s,
        operation(
            "directCall",
            json!({"name":"hive_status","args":{"operationId":"outer"}}),
        ),
    );
    let r = compare(&s);
    for i in [all, plain, metadata, history, status] {
        success(&r, i);
    }
    assert_eq!(value(&r, plain)["qualifiedName"], "hive.objects.read");
    assert_eq!(value(&r, metadata)["qualifiedName"], "hive.objects.stat");
    assert_eq!(value(&r, history)["qualifiedName"], "hive.objects.history");
    assert!(value(&r, metadata)["arguments"].get("view").is_none());
    assert!(value(&r, status).get("serviceNodeId").is_none());
    assert_eq!(code(&r, invalid), "invalid_arguments");
    assert_eq!(code(&r, outer), "invalid_arguments");
    assert!(value(&r, all).as_array().unwrap().len() > 20);
}

#[test]
fn mcp_service_calls_require_exact_owner_one_variant_and_preserve_inner_operation_identity_match_original()
 {
    let mut s = setup();
    let mut schema = input("string");
    schema["properties"]["operationId"] = json!({"type":"string"});
    schema["required"] = json!(["value", "operationId"]);
    let b = binding(
        "sample_write",
        "fixture.write",
        schema,
        json!({"type":"null"}),
        false,
        Some("node"),
    );
    let good = push(
        &mut s,
        call(
            vec![b.clone()],
            json!({"serviceNodeId":"node","input":{"value":"ok","operationId":"inner"}}),
        ),
    );
    let empty_id = push(
        &mut s,
        call(
            vec![b.clone()],
            json!({"serviceNodeId":"node","input":{"value":"ok","operationId":""}}),
        ),
    );
    let outer = push(
        &mut s,
        call(
            vec![b.clone()],
            json!({"serviceNodeId":"node","input":{"value":"ok","operationId":"inner"},"operationId":"outer"}),
        ),
    );
    let duplicate = push(
        &mut s,
        call(
            vec![b.clone(), b.clone()],
            json!({"serviceNodeId":"node","input":{"value":"ok","operationId":"inner"}}),
        ),
    );
    let absent = push(
        &mut s,
        call(
            vec![b],
            json!({"input":{"value":"ok","operationId":"inner"}}),
        ),
    );
    let mut agent = binding(
        "agent_manager_write",
        "fixture.write",
        input("string"),
        json!({"type":"null"}),
        false,
        Some("node"),
    );
    agent["serviceName"] = json!("agent-manager");
    let description = push(&mut s, descriptors(vec![agent]));
    let r = compare(&s);
    for i in [good, empty_id, description] {
        success(&r, i);
    }
    assert_eq!(value(&r, good)["operationId"], "inner");
    assert_eq!(value(&r, good)["arguments"]["operationId"], "inner");
    assert_eq!(value(&r, empty_id)["operationId"], "");
    for i in [outer, duplicate, absent] {
        assert_eq!(code(&r, i), "invalid_arguments");
    }
    assert!(
        value(&r, description)[0]["inputSchema"]["properties"]["operationId"]["description"]
            .as_str()
            .unwrap()
            .contains("<runtimeEpoch>:<issuedAtUnixMs>:<nonce>")
    );
}

#[test]
fn mcp_annotations_acknowledgements_description_precedence_and_output_unions_match_original() {
    let mut s = setup();
    let mut b = binding(
        "sample_ack",
        "fixture.ack",
        input("string"),
        json!({"type":"null"}),
        true,
        Some("a"),
    );
    let mut rows = vec![];
    for output in [
        json!({"type":"null"}),
        json!({"const":null}),
        json!({"type":"object","properties":{},"required":[],"patternProperties":{},"additionalProperties":false,"title":"Ack","$defs":{"Value":{"type":"string"}}}),
        json!({"type":"object","additionalProperties":false,"minProperties":0}),
        json!(false),
    ] {
        b["definition"]["outputSchema"] = output;
        rows.push(push(&mut s, descriptors(vec![b.clone()])));
    }
    let mut first = b.clone();
    first["definition"]["annotations"] = json!({"title":"Declared","readOnlyHint":true,"destructiveHint":false,"openWorldHint":false,"idempotentHint":true});
    first["definition"]["discovery"]["summary"] = json!("Summary");
    first["definition"]["outputSchema"] = json!({"type":"string"});
    let mut second = first.clone();
    second["provider"]["serviceNodeId"] = json!("b");
    second["definition"]["annotations"]["readOnlyHint"] = json!(false);
    second["definition"]["outputSchema"] = json!({"type":"number"});
    let union = push(&mut s, descriptors(vec![first, second]));
    let r = compare(&s);
    for i in rows.iter().copied().chain([union]) {
        success(&r, i);
    }
    for i in &rows[..3] {
        assert!(value(&r, *i)[0].get("outputSchema").is_none());
    }
    for i in &rows[3..] {
        assert!(value(&r, *i)[0].get("outputSchema").is_some());
    }
    let v = &value(&r, union)[0];
    assert_eq!(v["title"], "Declared");
    assert_eq!(v["description"], "Summary");
    assert_eq!(
        v["annotations"],
        json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})
    );
    assert_eq!(
        v["outputSchema"]["properties"]["result"]["anyOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn mcp_declared_presentation_metadata_and_raw_utf16_fallback_titles_match_original() {
    let mut s = setup();
    let metadata: Value = serde_json::from_str(include_str!(
        "../../services/hive/rust/src/mcp_metadata.json"
    ))
    .unwrap();
    let mut bindings = vec![];
    for (service, presentations) in metadata["presentations"].as_object().unwrap() {
        for (name, _) in presentations.as_object().unwrap() {
            let mut b = binding(
                name,
                "fixture.tool",
                json!({"type":"object","additionalProperties":false}),
                json!({"type":"null"}),
                true,
                if service == "hive" {
                    None
                } else {
                    Some("node")
                },
            );
            b["serviceName"] = json!(service);
            bindings.push(b);
        }
    }
    let known = push(&mut s, descriptors(bindings));
    let mut fallback = descriptors(vec![
        binding(
            "sample_unknown",
            "fixture.first",
            input("string"),
            json!({"type":"null"}),
            true,
            Some("node"),
        ),
        binding(
            "placeholder",
            "fixture.second",
            input("string"),
            json!({"type":"null"}),
            true,
            Some("node"),
        ),
        binding(
            "other_placeholder",
            "fixture.third",
            input("string"),
            json!({"type":"null"}),
            true,
            Some("node"),
        ),
    ]);
    fallback["parameterUnits"] = json!({"/bindings/1/definition/discovery/mcp/name":[0xdf,95,97],"/bindings/2/definition/discovery/mcp/name":[0xd800,95,98]});
    fallback["lossless"] = json!(true);
    let fallback = push(&mut s, fallback);
    let r = compare(&s);
    success(&r, known);
    success(&r, fallback);
    assert_eq!(value(&r, known).as_array().unwrap().len(), 84);
    let v =
        ivy_contracts::json::parse(value(&r, fallback)["encoded"].as_str().unwrap(), usize::MAX)
            .unwrap();
    assert_eq!(
        v.as_array().unwrap()[1]
            .get("title")
            .unwrap()
            .as_str()
            .unwrap(),
        "SS a"
    );
    assert_eq!(
        v.as_array().unwrap()[2]
            .get("title")
            .unwrap()
            .as_js_string()
            .unwrap()
            .0,
        vec![0xd800, 32, 98]
    );
}

#[test]
fn mcp_public_tool_schema_uses_actual_provider_catalog_and_hides_development_only_surfaces_match_original()
 {
    let mut s = setup();
    let a = defined("read", "sample_read", "ivy", "string", true);
    let mut b = a.clone();
    b["outputSchema"] = json!({"type":"object","properties":{"value":{"type":"string"}}});
    ready(
        &mut s,
        "a",
        vec![
            a.clone(),
            defined("dev", "sample_dev", "ivy_dev", "string", true),
        ],
    );
    ready(&mut s, "b", vec![b]);
    push(&mut s, action("disconnect", Some("a"), json!({})));
    let public = push(
        &mut s,
        operation("toolSchema", json!({"name":"sample_read"})),
    );
    let core = push(
        &mut s,
        operation("toolSchema", json!({"name":"hive_object_read"})),
    );
    let dev = push(
        &mut s,
        operation("toolSchema", json!({"name":"sample_dev"})),
    );
    let absent = push(&mut s, operation("toolSchema", json!({"name":"absent"})));
    let mut changed = a;
    changed["discovery"]["mcp"] = json!({"name":"sample_read_new","surface":"ivy"});
    ready(&mut s, "b", vec![changed]);
    let obsolete = push(
        &mut s,
        operation("toolSchema", json!({"name":"sample_read"})),
    );
    let next = push(
        &mut s,
        operation("toolSchema", json!({"name":"sample_read_new"})),
    );
    let r = compare(&s);
    for i in [public, core, next] {
        success(&r, i);
    }
    assert_eq!(
        value(&r, public)["inputSchema"]["properties"]["serviceNodeId"]["enum"],
        json!(["a", "b"])
    );
    assert_eq!(value(&r, public)["complete"], true);
    assert!(
        value(&r, public)["schemaHash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    for i in [dev, absent, obsolete] {
        assert_eq!(code(&r, i), "not_found");
    }
}

#[test]
fn mcp_conflicting_owners_invalid_selected_core_and_unused_output_reference_errors_match_original()
{
    let mut s = setup();
    let a = binding(
        "sample_tool",
        "fixture.read",
        input("string"),
        json!({"type":"null"}),
        true,
        Some("a"),
    );
    let mut other = a.clone();
    other["serviceName"] = json!("other");
    let conflict = push(&mut s, descriptors(vec![a.clone(), other]));
    let mut core = a.clone();
    core["provider"] = Value::Null;
    let mixed = push(&mut s, descriptors(vec![a.clone(), core]));
    let selected = binding(
        "hive_object_read",
        "hive.objects.stat",
        json!({"type":"object","additionalProperties":false}),
        json!({"type":"null"}),
        true,
        None,
    );
    let missing = push(&mut s, descriptors(vec![selected]));
    let mut invalid = a;
    invalid["definition"]["outputSchema"] =
        json!({"type":"null","$defs":{"Bad":{"$ref":"#/$defs/missing"}}});
    let ignored = push(&mut s, descriptors(vec![invalid]));
    let r = compare(&s);
    for i in [conflict, mixed, missing, ignored] {
        assert_eq!(code(&r, i), "registry_invalid");
    }
}

#[test]
fn mcp_surface_projection_defaults_to_development_and_hides_shadowed_public_names_match_original() {
    let mut s = setup();
    let public = binding(
        "sample_public",
        "fixture.read",
        input("string"),
        json!({"type":"null"}),
        true,
        Some("a"),
    );
    let mut shadow = public.clone();
    shadow["provider"]["serviceNodeId"] = json!("b");
    shadow["definition"]["discovery"]["mcp"]["surface"] = json!("ivy_dev");
    let mut implicit = binding(
        "sample_implicit",
        "fixture.internal",
        input("string"),
        json!({"type":"null"}),
        true,
        Some("b"),
    );
    implicit["definition"]["discovery"]["mcp"]
        .as_object_mut()
        .unwrap()
        .remove("surface");
    let mut explicit = implicit.clone();
    explicit["definition"]["discovery"]["mcp"] =
        json!({"name":"sample_explicit","surface":"ivy_dev"});
    let bindings = vec![public, shadow, implicit, explicit];
    let ivy = push(
        &mut s,
        operation("mcpSurface", json!({"surface":"ivy","bindings":bindings})),
    );
    let dev = push(
        &mut s,
        operation(
            "mcpSurface",
            json!({"surface":"ivy_dev","bindings":bindings}),
        ),
    );
    let r = compare(&s);
    success(&r, ivy);
    success(&r, dev);
    assert_eq!(value(&r, ivy).as_array().unwrap().len(), 1);
    assert_eq!(value(&r, dev).as_array().unwrap().len(), 2);
    assert_eq!(
        value(&r, dev)[0]["definition"]["discovery"]["mcp"]["name"],
        "sample_implicit"
    );
}

#[test]
fn mcp_success_keeps_acknowledgements_structured_text_and_png_metadata_projection_match_original() {
    let mut s = setup();
    let plain = push(
        &mut s,
        operation(
            "mcpSuccess",
            json!({"structured":true,"result":{"value":"é😀","items":[1,null]}}),
        ),
    );
    let ack = push(
        &mut s,
        operation(
            "mcpSuccess",
            json!({"structured":false,"result":{"ignored":true}}),
        ),
    );
    let mut projected = vec![];
    for data in [
        "iVBORw0KGgo",
        "iVBORw0KGgo=",
        "iVBORw0KGgo==",
        "iVBORw0KGgo===",
        "iVBORw0KGgo-",
        "iVBORw0KGgo\n",
        "iVBORw0KGgo\r",
        "iVBORw0KGgo\u{2028}",
        "iVBOR=w0KGgo",
        "other",
    ] {
        projected.push(push(&mut s,operation("mcpSuccess",json!({"structured":true,"result":{"mimeType":"image/png","encoding":"base64","data":data,"width":10,"nested":{"keep":true}}}))));
    }
    let wrong_type = push(
        &mut s,
        operation(
            "mcpSuccess",
            json!({"structured":true,"result":{"mimeType":"image/jpeg","encoding":"base64","data":"iVBORw0KGgo"}}),
        ),
    );
    let scalar = push(
        &mut s,
        operation("mcpSuccess", json!({"structured":true,"result":null})),
    );
    let mut raw = operation(
        "mcpSuccess",
        json!({"structured":true,"result":{"data":"placeholder"}}),
    );
    raw["parameterUnits"] = json!({"/result/data":[0xd800,97]});
    raw["lossless"] = json!(true);
    let raw = push(&mut s, raw);
    let r = compare(&s);
    for i in projected
        .iter()
        .copied()
        .chain([plain, ack, wrong_type, scalar, raw])
    {
        success(&r, i);
    }
    assert_eq!(value(&r, ack), &json!({"content":[]}));
    for i in &projected[..3] {
        assert_eq!(value(&r, *i)["content"].as_array().unwrap().len(), 2);
        assert!(
            value(&r, *i)["structuredContent"]["result"]
                .get("data")
                .is_none()
        );
        assert_eq!(
            value(&r, *i)["structuredContent"]["result"]["nested"]["keep"],
            true
        );
    }
    for i in projected[3..].iter().copied().chain([wrong_type]) {
        assert_eq!(value(&r, i)["content"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        value(&r, scalar)["structuredContent"],
        json!({"result":null})
    );
}
