use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    json::{parse, stringify},
};
use ivy_hive::{
    admission::{RequestBudget, RequestBudgetOptions},
    http_common::{self, Requests},
    http_response::{self, Response},
    http_static::{AssetResponse, Assets},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

fn json(text: &str) -> IvyJson {
    parse(text, usize::MAX).unwrap()
}
fn encode(value: &IvyJson) -> String {
    stringify(value, usize::MAX).unwrap()
}
fn obj(pairs: impl IntoIterator<Item = (&'static str, IvyJson)>) -> IvyJson {
    IvyJson::object(pairs)
}
fn text(input: &IvyJson, key: &str) -> JsString {
    input
        .get(key)
        .and_then(IvyJson::as_js_string)
        .cloned()
        .unwrap_or_default()
}
fn optional<'a>(input: &'a IvyJson, key: &str) -> Option<&'a JsString> {
    input.get(key).and_then(IvyJson::as_js_string)
}
fn flag(input: &IvyJson, key: &str) -> bool {
    input.get(key) == Some(&true.into())
}
fn failure(input: &IvyJson) -> Option<IvyError> {
    let code = optional(input, "code")?;
    let message = optional(input, "message")
        .cloned()
        .unwrap_or_else(|| "Isolated HTTP failure.".into());
    let mut error = IvyError::new_js(code.clone(), message);
    if let Some(outcome) = input.get("outcome") {
        error = error.with_provider_outcome(outcome.clone());
    }
    if let Some(details) = input.get("details") {
        error = error.with_details(details.clone());
    }
    Some(error)
}
#[derive(Default)]
struct Reply {
    events: Vec<IvyJson>,
    ended: bool,
    destroyed: bool,
    sent: bool,
    throw_header: JsString,
    throw_head: bool,
}
impl Reply {
    fn new(input: &IvyJson) -> Self {
        Self {
            ended: flag(input, "ended"),
            destroyed: flag(input, "destroyed"),
            sent: flag(input, "sent"),
            throw_header: text(input, "throwHeader"),
            throw_head: flag(input, "throwHead"),
            ..Self::default()
        }
    }
    fn events(&self) -> IvyJson {
        IvyJson::Array(self.events.clone())
    }
}
impl Response for Reply {
    fn writable_ended(&self) -> bool {
        self.ended
    }
    fn destroyed(&self) -> bool {
        self.destroyed
    }
    fn headers_sent(&self) -> bool {
        self.sent
    }
    fn set_header(&mut self, name: &str, value: &JsString) -> IvyResult<()> {
        self.events.push(obj([
            ("kind", "set".into()),
            ("name", name.into()),
            ("value", IvyJson::String(value.clone())),
        ]));
        if self.throw_header == JsString::from(name) {
            Err(IvyError::internal())
        } else {
            Ok(())
        }
    }
    fn write_head(&mut self, status: u16, headers: IvyJson) -> IvyResult<()> {
        self.events.push(obj([
            ("kind", "head".into()),
            ("status", IvyJson::Number(status as f64)),
            ("headers", headers),
        ]));
        if self.throw_head {
            return Err(IvyError::internal());
        }
        self.sent = true;
        Ok(())
    }
    fn end(&mut self, value: String) -> IvyResult<()> {
        self.end_bytes(value.as_bytes())
    }
    fn end_empty(&mut self) -> IvyResult<()> {
        self.events.push(obj([("kind", "end".into())]));
        self.ended = true;
        Ok(())
    }
    fn destroy(&mut self) -> IvyResult<()> {
        self.events.push(obj([("kind", "destroy".into())]));
        self.destroyed = true;
        Ok(())
    }
}
impl AssetResponse for Reply {
    fn end_bytes(&mut self, bytes: &[u8]) -> IvyResult<()> {
        self.events.push(obj([
            ("kind", "bytes".into()),
            ("base64", STANDARD.encode(bytes).into()),
        ]));
        self.ended = true;
        Ok(())
    }
}
fn error_result(error: IvyError, response: &Reply) -> IvyJson {
    obj([("error", error.to_wire()), ("events", response.events())])
}
async fn native(input: &IvyJson) -> IvyJson {
    let action = text(input, "action");
    let mut reply = Reply::new(input.get("response").unwrap_or(&IvyJson::Null));
    if action == JsString::from("parse") {
        return match http_common::json_body(
            &STANDARD
                .decode(text(input, "base64").to_utf8().unwrap())
                .unwrap(),
        ) {
            Ok(value) => obj([("value", value)]),
            Err(error) => error_result(error, &reply),
        };
    }
    if action == JsString::from("manifest") {
        return obj([(
            "encoded",
            encode(&ivy_hive::http_static::manifest(&text(input, "basePath"))).into(),
        )]);
    }
    if action == JsString::from("admission") {
        return admissions(input);
    }
    let origin = optional(input, "origin")
        .cloned()
        .unwrap_or_else(|| "https://example.test".into());
    let base = optional(input, "basePath")
        .cloned()
        .unwrap_or_else(|| "/ivy".into());
    if action == JsString::from("error") {
        let error = input.get("failure").and_then(failure);
        return match http_common::error(
            &origin,
            &base,
            &text(input, "path"),
            input.get("id").cloned().unwrap_or(IvyJson::Null),
            error.as_ref(),
            &mut reply,
        ) {
            Ok(()) => obj([("events", reply.events())]),
            Err(error) => error_result(error, &reply),
        };
    }
    let requests = Requests::default();
    let mut request = match requests.begin(1, &mut reply) {
        Ok(value) => value,
        Err(error) => return error_result(error, &reply),
    };
    let mut path = JsString::default();
    let mut value = IvyJson::Null;
    let mut handled = true;
    let result = async {
        request.admit(&"client".into())?;
        let route = http_common::route(
            optional(input, "method"),
            optional(input, "url"),
            &origin,
            &base,
        )?;
        if route.authorization_metadata {
            return http_response::json(
                &mut reply,
                200,
                &obj([("authorizationMetadata", true.into())]),
            );
        }
        path = route.path;
        if action == JsString::from("assets") {
            let assets = Assets {
                console_root: optional(input, "root")
                    .map(|p| ivy_host_runtime::node_path::to_native(p)),
            };
            handled = assets
                .public(optional(input, "method"), &path, &base, &mut reply)
                .await?
                || assets
                    .console(
                        optional(input, "method"),
                        &route.pathname,
                        &path,
                        &base,
                        &mut reply,
                    )
                    .await?;
        } else {
            value = obj([
                ("href", route.url.href.into()),
                ("pathname", IvyJson::String(route.pathname)),
                ("authorizationMetadata", false.into()),
                ("path", IvyJson::String(path.clone())),
            ]);
        }
        Ok::<_, IvyError>(())
    }
    .await;
    if let Err(error) = result {
        if let Err(error) = http_common::error(
            &origin,
            &base,
            &path,
            IvyJson::Null,
            Some(&error),
            &mut reply,
        ) {
            request.release();
            return error_result(error, &reply);
        }
    }
    request.release();
    assert_eq!(requests.active(1), 0);
    if action == JsString::from("assets") {
        obj([("handled", handled.into()), ("events", reply.events())])
    } else {
        obj([("value", value), ("events", reply.events())])
    }
}
fn admissions(input: &IvyJson) -> IvyJson {
    let b = input
        .get("budget")
        .and_then(IvyJson::as_array)
        .unwrap_or(&[]);
    let n = |at: usize, default: f64| b.get(at).and_then(IvyJson::as_f64).unwrap_or(default);
    let owner = Requests::new(RequestBudget::new(RequestBudgetOptions {
        maximum: (n(0, 2048.0)) as usize,
        maximum_bytes: (n(1, 128.0 * 1024.0 * 1024.0)) as usize,
        per_consumer: (n(2, 256.0)) as usize,
        per_consumer_bytes: (n(3, 64.0 * 1024.0 * 1024.0)) as usize,
        name: b
            .get(4)
            .map(|value| value.as_str().unwrap())
            .unwrap_or_else(|| "request".into()),
    }));
    let mut sockets = vec![];
    let mut pending = BTreeMap::new();
    let mut replies: BTreeMap<JsString, Reply> = BTreeMap::new();
    let mut reply_order = vec![];
    let mut snapshots = vec![];
    for step in input.required("steps").unwrap().as_array().unwrap() {
        let id = text(step, "id");
        match text(step, "op").to_utf8().unwrap().as_str() {
            "start" => {
                let socket = step.required("socket").unwrap().as_f64().unwrap() as u64;
                if !sockets.contains(&socket) {
                    sockets.push(socket);
                }
                let mut res = Reply::new(step.get("response").unwrap_or(&IvyJson::Null));
                match owner.begin(socket, &mut res) {
                    Err(error) => res
                        .events
                        .push(obj([("kind", "throw".into()), ("error", error.to_wire())])),
                    Ok(mut request) => {
                        let admitted = if flag(step, "addressError") {
                            Err(IvyError::new(
                                "unauthenticated",
                                "Isolated address failure.",
                            ))
                        } else {
                            request.admit(optional(step, "client").unwrap_or(&"client".into()))
                        };
                        if let Err(error) = admitted {
                            http_common::error(
                                &"https://example.test".into(),
                                &"/ivy".into(),
                                &JsString::default(),
                                IvyJson::Null,
                                Some(&error),
                                &mut res,
                            )
                            .unwrap();
                            request.release();
                        } else {
                            pending.insert(id.clone(), request);
                        }
                    }
                }
                reply_order.push(id.clone());
                replies.insert(id, res);
            }
            "finish" => {
                pending.remove(&id).unwrap().release();
            }
            "grow" => {
                if let Err(error) = pending
                    .get_mut(&id)
                    .unwrap()
                    .body_lease()
                    .unwrap()
                    .grow((step.required("bytes").unwrap().as_f64().unwrap()) as usize)
                {
                    replies.get_mut(&id).unwrap().events.push(obj([
                        ("kind", "growError".into()),
                        ("error", error.to_wire()),
                    ]));
                }
            }
            "snapshot" => {
                let mut bodies: Vec<_> = pending.keys().cloned().collect();
                bodies.sort();
                snapshots.push(obj([
                    ("name", step.required("name").unwrap().clone()),
                    (
                        "active",
                        IvyJson::Array(
                            sockets
                                .iter()
                                .map(|socket| {
                                    IvyJson::Array(vec![
                                        IvyJson::Number(*socket as f64),
                                        IvyJson::Number(owner.active(*socket) as f64),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "bodies",
                        IvyJson::Array(bodies.into_iter().map(IvyJson::String).collect()),
                    ),
                    (
                        "events",
                        IvyJson::Object(
                            reply_order
                                .iter()
                                .map(|id| (id.clone(), replies[id].events()))
                                .collect(),
                        ),
                    ),
                ]));
            }
            _ => panic!("Unknown admission fixture action"),
        }
    }
    assert!(pending.is_empty());
    obj([("snapshots", IvyJson::Array(snapshots))])
}
fn oracle(inputs: &[IvyJson]) -> Vec<IvyJson> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child =
        Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
            .arg(root.join("tests/fixtures/hive-http-oracle.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(encode(&IvyJson::Array(inputs.to_vec())).as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX)
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec()
}
async fn compare(name: &str, inputs: &[IvyJson]) {
    let expected = oracle(inputs);
    assert_eq!(expected.len(), inputs.len());
    for (index, (input, expected)) in inputs.iter().zip(expected).enumerate() {
        let actual = native(input).await;
        if let Some(path) = std::env::var_os("IVY_HIVE_HTTP_CORPUS") {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let _lock = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
            let row = obj([
                ("test", name.into()),
                ("index", IvyJson::Number(index as f64)),
                ("input", input.clone()),
                ("expected", expected.clone()),
                ("actual", actual.clone()),
            ]);
            let line = encode(&row) + "\n";
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
        }
        // JSON.stringify also normalizes NaN/infinity and negative zero at this boundary.
        assert_eq!(
            json(&encode(&actual)),
            json(&encode(&expected)),
            "{name} row {index}: {}",
            encode(input)
        );
    }
}
#[tokio::test]
async fn original_common_url_admission_and_raw_json_body() {
    let mut inputs = vec![];
    for base in ["", "/ivy"] {
        for method in [None, Some("GET"), Some("POST")] {
            for url in [
                None,
                Some("/"),
                Some("/ivy"),
                Some("/ivy/"),
                Some("/ivy2/x"),
                Some("/ivy/../outside"),
                Some("/ivy/a%2Fb?x=1#hash"),
                Some("//elsewhere.test/ivy/x"),
                Some("https://elsewhere.test/ivy/x"),
                Some("/.well-known/oauth-authorization-server"),
                Some("http://[bad"),
                Some("/ivy/é/🙂"),
            ] {
                let mut row = obj([("action", "route".into()), ("basePath", base.into())]);
                if let Some(method) = method {
                    row.insert("method", method.into());
                }
                if let Some(url) = url {
                    row.insert("url", url.into());
                }
                inputs.push(row);
            }
        }
    }
    for source in [
        b"null".as_slice(),
        b"{\"a\":1,\"a\":2}",
        b"{\"__proto__\":true,\"s\":\"\\ud800\"}",
        b"-0",
        b"1e9999",
        b"9007199254740993",
        b"[true,false,null]",
        b"\xef\xbb\xbf{\"ok\":true}",
        b"\xef\xbb\xbf\xef\xbb\xbf{}",
        b"\xff",
        b"\xc0\xaf",
        b"\xed\xa0\x80",
        b"{\"x\":\"\x01\"}",
        b"",
        b" ",
        b"undefined",
        b"[] trailing",
        b"{\"x\":}",
    ] {
        inputs.push(obj([
            ("action", "parse".into()),
            ("base64", STANDARD.encode(source).into()),
        ]));
    }
    compare("common-url-json", &inputs).await;
}
#[tokio::test]
async fn original_mcp_oauth_and_rpc_error_effects() {
    let mut inputs = vec![];
    for path in [
        "",
        "/mcp",
        "/mcp-dev",
        "/oauth/token",
        "/oauth/register",
        "/other",
    ] {
        for code in [
            None,
            Some("unauthenticated"),
            Some("limit_exceeded"),
            Some("not_found"),
            Some("invalid_arguments"),
            Some("internal_error"),
        ] {
            for flags in [
                json("{}"),
                json(r#"{"ended":true}"#),
                json(r#"{"sent":true}"#),
            ] {
                let mut row = obj([
                    ("action", "error".into()),
                    ("path", path.into()),
                    ("id", IvyJson::Number(17.0)),
                    ("response", flags),
                ]);
                if let Some(code) = code {
                    row.insert(
                        "failure",
                        obj([
                            ("code", code.into()),
                            ("message", IvyJson::String(JsString(vec![0xd800, 0, 65]))),
                            ("outcome", "unknown".into()),
                            ("details", json(r#"{"retained":true}"#)),
                        ]),
                    );
                }
                inputs.push(row);
            }
        }
    }
    inputs.push(json(r#"{"action":"error","path":"/mcp","failure":{"code":"unauthenticated"},"response":{"throwHeader":"WWW-Authenticate"}}"#));
    inputs.push(json(r#"{"action":"error","path":"/oauth/token","failure":{"code":"not_found"},"response":{"destroyed":true}}"#));
    compare("error-effects", &inputs).await;
}
#[tokio::test]
async fn original_request_slots_cleanup_and_failure_order() {
    let mut steps = vec![];
    for index in 0..66 {
        steps.push(obj([
            ("op", "start".into()),
            ("id", format!("r{index:02}").into()),
            ("socket", IvyJson::Number(1.0)),
            ("client", format!("client{index}").into()),
        ]));
    }
    steps.push(json(r#"{"op":"snapshot","name":"connection-bound"}"#));
    steps.push(json(r#"{"op":"finish","id":"r01"}"#));
    steps.push(json(
        r#"{"op":"start","id":"reuse","socket":1,"client":"fresh"}"#,
    ));
    steps.push(json(r#"{"op":"start","id":"header-failure","socket":2,"response":{"throwHeader":"X-Frame-Options"}}"#));
    steps.push(json(
        r#"{"op":"start","id":"address-failure","socket":2,"addressError":true}"#,
    ));
    steps.push(json(r#"{"op":"snapshot","name":"out-of-order-reuse"}"#));
    for index in (0..64).rev() {
        if index != 1 {
            steps.push(obj([
                ("op", "finish".into()),
                ("id", format!("r{index:02}").into()),
            ]));
        }
    }
    steps.push(json(r#"{"op":"finish","id":"reuse"}"#));
    steps.push(json(r#"{"op":"snapshot","name":"released"}"#));
    let a = obj([
        ("action", "admission".into()),
        ("steps", IvyJson::Array(steps)),
    ]);
    let b = json(
        r#"{"action":"admission","budget":[2,2048,1,1024],"steps":[{"op":"start","id":"one","socket":1},{"op":"start","id":"consumer-full","socket":2},{"op":"start","id":"two","socket":3,"client":"other"},{"op":"start","id":"global-full","socket":4,"client":"third"},{"op":"grow","id":"one","bytes":1},{"op":"snapshot","name":"charged-through-error"},{"op":"finish","id":"one"},{"op":"start","id":"reused","socket":2},{"op":"finish","id":"two"},{"op":"finish","id":"reused"},{"op":"snapshot","name":"released"}]}"#,
    );
    let c = json(
        r#"{"action":"admission","budget":[2048,134217728,64,67108864],"steps":[{"op":"start","id":"one","socket":1},{"op":"grow","id":"one","bytes":2048},{"op":"finish","id":"one"},{"op":"snapshot","name":"body-release"}]}"#,
    );
    compare("admission-order", &[a, b, c]).await;
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("ivy-hive-http-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let path = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            path.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-hive-http-")
        );
        fs::remove_dir_all(path).unwrap();
    }
}
#[path = "../fixtures/hive-native-static.rs"]
mod native_static;
