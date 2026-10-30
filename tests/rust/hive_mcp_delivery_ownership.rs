use super::*;
use crate::retention::graph_tests::{LIVE, PEAK};
use std::sync::atomic::Ordering;

fn definition(v: &IvyJson) -> IvyResult<&IvyJson> { v.required("definition") }
fn field<'a>(v: &'a IvyJson, key: &str) -> IvyResult<&'a IvyJson> { v.required(key) }
fn is_text(v: Option<&IvyJson>, text: &str) -> bool {
    v.and_then(IvyJson::as_js_string).is_some_and(|v| v.eq_str(text))
}
fn text_pointer(value: &IvyJson) -> usize {
    value.as_js_string().unwrap().0.as_ptr() as usize
}
fn binding(surface: Option<&str>, name: &str, payload: IvyJson) -> IvyJson {
    let mut mcp = IvyJson::object([("name", name.into())]);
    if let Some(surface) = surface { mcp.insert("surface", surface.into()); }
    IvyJson::object([("definition", IvyJson::object([
        ("discovery", IvyJson::object([("mcp", mcp)])),
        ("inputSchema", payload),
    ]))])
}
struct CatalogStorage { value: Option<IvyJson>, requests: Vec<IvyJson> }
impl Storage for CatalogStorage {
    async fn request(&mut self, request: IvyJson) -> IvyResult<IvyJson> {
        self.requests.push(request);
        Ok(self.value.take().unwrap())
    }
}
fn storage(value: IvyJson) -> CatalogStorage { CatalogStorage { value: Some(value), requests: vec![] } }
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().build().unwrap()
}
fn png(data: IvyJson) -> IvyJson {
    IvyJson::object([("mimeType", "image/png".into()), ("encoding", "base64".into()), ("data", data)])
}

// Frozen borrowed projections retain the previous byte and allocation behavior.
fn reference_surface_bindings(bindings: &[IvyJson], surface: &JsString) -> IvyResult<Vec<IvyJson>> {
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

fn reference_success(result: &IvyJson, structured: bool) -> IvyResult<IvyJson> {
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

#[test]
fn catalog_and_surface_selection_transfer_owned_payloads() {
    let rt = runtime();
    let payload = IvyJson::object([(JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801]))) ]);
    let fixture = vec![binding(Some("ivy"), "shared", payload.clone()),
        binding(Some("ivy_dev"), "shared", payload.clone()), binding(None, "private", payload.clone()),
        binding(Some("ivy_dev"), "other", payload), binding(Some("other"), "hidden", true.into())];
    for surface in ["ivy", "ivy_dev", "other"] {
        let expected = reference_surface_bindings(&fixture, &surface.into()).unwrap();
        let mut input = storage(IvyJson::Array(fixture.clone()));
        let actual = rt.block_on(catalog(&mut input, None)).unwrap();
        let actual = mcp::surface_bindings(actual, &surface.into()).unwrap();
        assert_eq!(stringify(&IvyJson::Array(actual.clone()), usize::MAX).unwrap(), stringify(&IvyJson::Array(expected), usize::MAX).unwrap());
        assert_eq!(input.requests, vec![IvyJson::object([("action", "mcp.catalog".into())])]);
    }
    for name in [None, Some(JsString::from("")), Some(JsString::from("tool"))] {
        let payload = IvyJson::String(JsString(vec![0xd800, 0xd801]));
        let pointer = text_pointer(&payload);
        let mut input = storage(IvyJson::Array(vec![binding(Some("ivy"), "tool", payload)]));
        let result = rt.block_on(catalog(&mut input, name.as_ref())).unwrap();
        let result = mcp::surface_bindings(result, &"ivy".into()).unwrap();
        assert_eq!(text_pointer(result[0].required("definition").unwrap().required("inputSchema").unwrap()), pointer);
        assert_eq!(input.requests[0].get("name").is_some(), name.as_ref().is_some_and(|n| !n.0.is_empty()));
        if let Some(name) = name.filter(|n| !n.0.is_empty()) { assert_eq!(input.requests[0].get("name"), Some(&name.into())); }
    }
    for value in [IvyJson::Null, true.into(), empty()] {
        assert!(rt.block_on(catalog(&mut storage(value), None)).is_err());
    }
}

#[test]
fn tool_success_preserves_bytes_and_owned_opaque_or_image_buffers() {
    let mut duplicate = png("iVBORw0KGgoAAAA=".into()).into_object().unwrap();
    duplicate.push(("data".into(), "opaque duplicate".into()));
    duplicate.push((JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801]))));
    let mut wrong_mime = png("iVBORw0KGgoAAAA=".into()); wrong_mime.insert("mimeType", "image/jpeg".into());
    let mut wrong_encoding = png("iVBORw0KGgoAAAA=".into()); wrong_encoding.insert("encoding", "hex".into());
    let fixtures = vec![IvyJson::Null, true.into(), 12.0.into(), "opaque".into(), empty(),
        IvyJson::object([(JsString(vec![0xd800]), IvyJson::String(JsString(vec![0xd801])))]),
        png("iVBORw0KGgoAAAA=".into()), png("iVBORw0KGgoAAAA==".into()),
        png("iVBORw0KGgoA=A".into()), png("iVBORw0KGgo===".into()), png("iVBORw0KGgo?".into()),
        png("not png".into()), png(false.into()), wrong_mime, wrong_encoding, IvyJson::Object(duplicate)];
    for input in fixtures { for structured in [false, true] {
        let expected = reference_success(&input, structured).unwrap();
        let actual = mcp::success(input.clone(), structured).unwrap();
        assert_eq!(stringify(&actual, usize::MAX).unwrap(), stringify(&expected, usize::MAX).unwrap());
    } }
    for image in [false, true] {
        let input = if image { png("iVBORw0KGgoAAAA=".into()) } else { IvyJson::object([("payload", "owned opaque".into())]) };
        let pointer = text_pointer(input.required(if image { "data" } else { "payload" }).unwrap());
        let output = mcp::success(input, true).unwrap();
        let data = if image { output.required("content").unwrap().as_array().unwrap()[1].required("data").unwrap() }
            else { output.required("structuredContent").unwrap().required("result").unwrap().required("payload").unwrap() };
        assert_eq!(text_pointer(data), pointer);
    }
}

#[test]
fn catalog_filter_and_png_success_do_not_copy_large_payloads() {
    const CHILD: &str = "IVY_MCP_DELIVERY_MEMORY_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp_request::ownership_tests::catalog_filter_and_png_success_do_not_copy_large_payloads", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
        return;
    }
    let rt = runtime();
    for kind in ["catalog+surface", "png success"] { for original in [true, false] {
        let input = if kind == "png success" { png(("iVBORw0KGgo".to_owned() + &"A".repeat(1024 * 1024)).as_str().into()) }
            else { IvyJson::Array(vec![binding(Some("ivy"), "tool", "S".repeat(1024 * 1024).as_str().into())]) };
        let mut source = storage(input);
        let surface = JsString::from("ivy");
        let before = LIVE.load(Ordering::Relaxed); PEAK.store(before, Ordering::Relaxed);
        let output = if kind == "png success" {
            let input = source.value.take().unwrap();
            if original { reference_success(&input, true).unwrap() } else { mcp::success(input, true).unwrap() }
        } else {
            let result = if original {
                let owned = {
                    let result = rt.block_on(source.request(empty())).unwrap();
                    result.as_array().unwrap().to_vec()
                };
                reference_surface_bindings(&owned, &surface).unwrap()
            } else {
                let owned = rt.block_on(catalog(&mut source, None)).unwrap();
                mcp::surface_bindings(owned, &surface).unwrap()
            };
            IvyJson::Array(result)
        };
        let peak = PEAK.load(Ordering::Relaxed) - before;
        eprintln!("Hive MCP delivery Rust allocation peak: {peak} bytes; kind={kind}; original={original}; one 1 MiB payload");
        if !original { assert!(peak < 64 * 1024, "Delivery copied payload: {peak}"); }
        drop(output);
    } }
}
