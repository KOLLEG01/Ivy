use super::*;

// Frozen pre-ownership projection for byte-level protocol comparisons.
fn reference_ordered(value: &IvyJson, fields: &[&str]) -> IvyJson {
    let mut result = IvyJson::object(
        fields
            .iter()
            .filter_map(|name| value.get(name).map(|value| (*name, value.clone()))),
    );
    for (name, value) in value.as_object().unwrap_or(&[]) {
        if result.get_key(name).is_none() {
            result.insert(name.clone(), value.clone());
        }
    }
    result
}
fn reference_encode_result(method: &JsString, mut value: IvyJson, modern: bool, info: &IvyJson) -> IvyJson {
    // The SDK projects application tool results through its declared schema order.
    if method == &JsString::from("tools/call") {
        if let Some(mut content) = value
            .get("content")
            .and_then(IvyJson::as_array)
            .map(<[_]>::to_vec)
        {
            for item in &mut content {
                *item = reference_ordered(
                    item,
                    if item.get("type") == Some(&"image".into()) {
                        &["type", "data", "mimeType", "annotations", "_meta"]
                    } else {
                        &["type", "text", "annotations", "_meta"]
                    },
                );
            }
            value.insert("content", IvyJson::Array(content));
        }
        value = reference_ordered(
            &value,
            &["_meta", "content", "structuredContent", "isError"],
        );
    }
    if !modern {
        return value;
    }
    if method == &JsString::from("tools/list") {
        if let Some(mut tools) = value.get("tools").cloned() {
            if let IvyJson::Array(items) = &mut tools {
                for tool in items {
                    tool.remove("execution");
                }
            }
            value.insert("tools", tools);
        }
    }
    if let Some(mut capabilities) = value.get("capabilities").cloned() {
        capabilities.remove("tasks");
        value.insert("capabilities", capabilities);
    }
    if value.get("resultType").is_none() {
        value.insert("resultType", "complete".into());
    }
    if method == &JsString::from("tools/list") || method == &JsString::from("server/discover") {
        if !value
            .get("ttlMs")
            .and_then(IvyJson::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0 && n.fract() == 0.0)
        {
            value.insert("ttlMs", IvyJson::Number(0.0));
        }
        if !value
            .get("cacheScope")
            .is_some_and(|v| v == &"public".into() || v == &"private".into())
        {
            value.insert("cacheScope", "private".into());
        }
    }
    let key = "io.modelcontextprotocol/serverInfo";
    match value.get("_meta").cloned() {
        Some(mut meta @ IvyJson::Object(_)) => {
            if meta.get(key).is_none() {
                meta.insert(key, info.clone());
            }
            value.insert("_meta", meta);
        }
        None => {
            value.insert("_meta", IvyJson::object([(key, info.clone())]));
        }
        _ => {}
    }
    value
}


fn serialized(value: &IvyJson) -> String {
    ivy_contracts::json::stringify(value, usize::MAX).unwrap()
}
fn parsed(text: &str) -> IvyJson {
    ivy_contracts::json::parse(text, usize::MAX).unwrap()
}
fn pointer(value: &IvyJson) -> *const u16 {
    value.as_js_string().unwrap().0.as_ptr()
}

#[test]
fn encoding_preserves_original_projection_and_protocol_fields() {
    let fixture = parsed(r#"{"unknown\ud800":{"number":9007199254740992,"units":"\udfff","negativeZero":-0},"isError":false,"structuredContent":{"opaque":[null,true,"text"]},"content":[{"extra":3,"_meta":{"opaque":1},"annotations":{"audience":[]},"text":"literal \\n\ud800","type":"text"},{"annotations":{},"mimeType":"image/png","data":"AA==","type":"image","unknown":null},null,7],"tools":[{"execution":{"taskSupport":"optional"},"outputSchema":{"type":"object"},"name":"fixture","inputSchema":{"type":"integer"}},false],"capabilities":{"tasks":{"x":1},"tools":{}},"ttlMs":-1,"cacheScope":false}"#);
    let info = parsed(r#"{"name":"fixture","version":"1"}"#);
    let metadata = [None, Some(IvyJson::Null), Some(true.into()), Some(IvyJson::Array(vec![])), Some(empty()), Some(parsed(r#"{"io.modelcontextprotocol/serverInfo":null,"opaque":"\ud800"}"#))];
    for method in ["tools/call", "tools/list", "server/discover", "initialize"] {
        for modern in [false, true] {
            for meta in &metadata {
                let mut input = fixture.clone();
                if let Some(meta) = meta { input.insert("_meta", meta.clone()); }
                let expected = reference_encode_result(&method.into(), input.clone(), modern, &info);
                let actual = encode_result(&method.into(), input, modern, &info);
                assert_eq!(serialized(&actual), serialized(&expected), "{method}, modern={modern}");
            }
        }
    }
    for input in [IvyJson::Null, true.into(), 3.0.into(), "text".into(), IvyJson::Array(vec![1.0.into()]), IvyJson::Object(vec![("z".into(), 1.0.into()), ("type".into(), "first".into()), ("type".into(), "second".into()), (JsString(vec![0xd800]), 4.0.into()), ("z".into(), 5.0.into())])] {
        assert_eq!(serialized(&ordered(input.clone(), &["type", "_meta"])), serialized(&reference_ordered(&input, &["type", "_meta"])));
    }
    for field in ["content", "tools", "capabilities", "_meta"] {
        for invalid_shape in [IvyJson::Null, 1.0.into(), "scalar".into(), IvyJson::Array(vec![])] {
            let mut input = fixture.clone(); input.insert(field, invalid_shape);
            for method in ["tools/call", "tools/list"] {
                let expected = reference_encode_result(&method.into(), input.clone(), true, &info);
                let actual = encode_result(&method.into(), input.clone(), true, &info);
                assert_eq!(serialized(&actual), serialized(&expected));
            }
        }
    }
}

fn payload(method: &str) -> IvyJson {
    let text = || IvyJson::String(JsString::from("x".repeat(1024 * 1024)));
    if method == "tools/call" {
        IvyJson::object([
            ("structuredContent", IvyJson::object([("opaque", text())])),
            ("content", IvyJson::Array(vec![IvyJson::object([("text", text()), ("type", "text".into())])])),
            ("_meta", IvyJson::object([("opaque", text())])),
        ])
    } else {
        IvyJson::object([
            ("tools", IvyJson::Array(vec![IvyJson::object([
                ("name", "fixture".into()), ("execution", empty()),
                ("outputSchema", IvyJson::object([("description", text())])),
            ])])),
            ("_meta", IvyJson::object([("opaque", text())])),
            ("capabilities", IvyJson::object([("tasks", empty()), ("opaque", text())])),
        ])
    }
}
fn pointers(value: &IvyJson, method: &str) -> Vec<*const u16> {
    let meta = pointer(value.get("_meta").unwrap().get("opaque").unwrap());
    if method == "tools/call" {
        vec![meta, pointer(value.get("structuredContent").unwrap().get("opaque").unwrap()), pointer(value.get("content").unwrap().as_array().unwrap()[0].get("text").unwrap())]
    } else {
        vec![meta, pointer(value.get("capabilities").unwrap().get("opaque").unwrap()), pointer(value.get("tools").unwrap().as_array().unwrap()[0].get("outputSchema").unwrap().get("description").unwrap())]
    }
}
#[test]
fn encoding_retains_owned_response_and_catalog_buffers() {
    for method in ["tools/call", "tools/list"] {
        for modern in [false, true] {
            let input = payload(method); let before = pointers(&input, method);
            let output = encode_result(&method.into(), input, modern, &empty());
            assert_eq!(pointers(&output, method), before);
        }
    }
}

#[test]
fn encoding_does_not_copy_large_response_or_catalog_payloads() {
    use crate::retention::graph_tests::{LIVE, PEAK};
    use std::sync::atomic::Ordering;
    const CHILD: &str = "IVY_MCP_ENCODING_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp_dispatch::ownership_tests::encoding_does_not_copy_large_response_or_catalog_payloads", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    for method in ["tools/call", "tools/list"] {
        let name = JsString::from(method); let info = empty();
        for original in [true, false] {
            let input = payload(method);
            let before = LIVE.load(Ordering::Relaxed); PEAK.store(before, Ordering::Relaxed);
            let output = if original { reference_encode_result(&name, input, true, &info) } else { encode_result(&name, input, true, &info) };
            let peak = PEAK.load(Ordering::Relaxed) - before;
            eprintln!("Hive MCP encoding Rust allocation peak: {peak} bytes; method={method}; original={original}; three 1 MiB text buffers");
            if !original { assert!(peak < 64 * 1024, "MCP encoding copied payload buffers: {peak}"); }
            drop(output);
        }
    }
}
