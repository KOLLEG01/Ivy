use super::*;
use ivy_hive::{
    server_lifecycle::{self, Listener, OAuth, StorageLifecycle},
    server_runtime::{self, Server},
    store::Credential,
};
pub(super) type Immediates = Arc<Mutex<VecDeque<Box<dyn FnOnce() + Send>>>>;

pub(super) fn credentials(value: &Value) -> Vec<Credential> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| Credential {
            principal_id: v["principalId"].as_str().unwrap().into(),
            digest: v["digest"].as_str().unwrap().into(),
        })
        .collect()
}
impl StorageLifecycle for Worker {
    fn start(&self) -> Reply<()> {
        let work = post(
            &self.controls,
            &self.events,
            "start".into(),
            IvyJson::object([("kind", "storage-start".into())]),
        );
        Box::pin(async move { work.await.map(|_| ()) })
    }
    fn close(&self) -> Reply<()> {
        let work = post(
            &self.controls,
            &self.events,
            "storage-close".into(),
            IvyJson::object([("kind", "storage-close".into())]),
        );
        Box::pin(async move { work.await.map(|_| ()) })
    }
}
struct Ports(Events);
impl OAuth for Ports {
    fn initialize(&self, value: &IvyJson) -> IvyResult<()> {
        self.0.lock().unwrap().push(IvyJson::object([
            ("kind", "oauth-initialize".into()),
            ("value", value.clone()),
        ]));
        Ok(())
    }
    fn set_credentials(&self, value: Vec<Credential>) -> IvyResult<()> {
        let value = IvyJson::Array(
            value
                .iter()
                .map(|v| {
                    IvyJson::object([
                        ("principalId", IvyJson::String(v.principal_id.clone())),
                        ("digest", IvyJson::String(v.digest.clone())),
                    ])
                })
                .collect(),
        );
        self.0.lock().unwrap().push(IvyJson::object([
            ("kind", "oauth-set".into()),
            ("value", value),
        ]));
        Ok(())
    }
}
impl server_lifecycle::Push for Ports {
    fn start(&self) -> IvyResult<()> {
        panic!("Composition checks do not start browser push")
    }
    fn close(&self) -> Reply<()> {
        panic!("Composition checks do not close browser push")
    }
}
impl server_runtime::Push for Ports {
    fn wake(&self) {
        self.0
            .lock()
            .unwrap()
            .push(IvyJson::object([("kind", "wake".into())]));
    }
}
impl Listener for Ports {
    fn listen(&self, _: f64, _: JsString) -> Reply<()> {
        panic!("Listener binding is outside this composition check")
    }
    fn address(&self) -> IvyResult<IvyJson> {
        panic!("Listener binding is outside this composition check")
    }
    fn close_websockets(&self) -> IvyResult<()> {
        panic!("Listener binding is outside this composition check")
    }
    fn close_connections(&self) -> IvyResult<()> {
        panic!("Listener binding is outside this composition check")
    }
    fn close(&self) -> Reply<()> {
        panic!("Listener binding is outside this composition check")
    }
}
pub(super) fn compose(
    trace: &Value,
    source: &IvyJson,
    base: Owners,
    storage: Arc<Worker>,
    packages: Option<PackageRegistry>,
    events: Events,
    _controls: Controls,
) -> (Server, Immediates) {
    let ports = Arc::new(Ports(events.clone()));
    let immediates = Immediates::default();
    let warn_events = events.clone();
    let queue = immediates.clone();
    let (server, canonical_base) = Server::configured(
        server_runtime::Owners {
            storage,
            oauth: ports.clone(),
            push: ports.clone(),
            packages,
            listener: ports,
            now: base.now,
            dates: Arc::new(super::super::TraceRuntime(
                source.get("profile").unwrap().clone(),
                events,
            )),
            ids: base.ids,
            provider_error: base.provider_error,
            clear_authentication: base.clear_authentication,
            immediate: Arc::new(move |work| queue.lock().unwrap().push_back(work)),
            unhandled: base.unhandled,
            warn: Arc::new(move |message| {
                warn_events.lock().unwrap().push(IvyJson::object([
                    ("kind", "warning".into()),
                    ("message", message.into()),
                ]))
            }),
        },
        server_runtime::Configuration {
            public_base_url: "https://hive.invalid/ivy".into(),
            trusted_proxy_addresses: vec![],
            runtime: server_runtime::Options {
                validate_messages: trace["options"]["validateMessages"] == true,
                call_timeout_ms: trace["options"]["callTimeoutMs"].as_f64(),
                package_publisher_principal_ids: trace["options"]["packagePublisherPrincipalIds"]
                    .as_array()
                    .map(|v| v.iter().map(|v| v.as_str().unwrap().into()).collect())
                    .unwrap_or_default(),
                invocation: InvocationOptions {
                    reset_bootstrap_credential_digest:
                        trace["options"]["resetBootstrapCredentialDigest"]
                            .as_str()
                            .map(Into::into),
                },
                lifecycle: server_lifecycle::Options {
                    credentials: credentials(&trace["options"]["credentials"]),
                    ..Default::default()
                },
            },
        },
        server_runtime::Admission {
            requests: base.requests,
            dispatch: base.dispatch,
        },
    )
    .unwrap();
    assert_eq!(canonical_base.base_path, JsString::from("/ivy"));
    (server, immediates)
}

fn composed(name: impl Into<String>, steps: Vec<Value>) -> Value {
    json!({"name":name.into(),"composed":true,"realDirect":true,"packages":false,"steps":steps})
}
fn invoke(name: &str, frame: Value) -> Value {
    let mut caller = context(None);
    caller["transport"] = json!("http");
    json!({"op":"invoke","name":name,"context":caller,"frame":frame})
}
fn tool(hash: char) -> Value {
    let mut params: Value = serde_json::from_str(&encode(&super::super::params())).unwrap();
    params["serviceNodeId"] = json!("node");
    params["expectedDefinitionHash"] = json!(format!("sha256:{}", hash.to_string().repeat(64)));
    json!({"jsonrpc":"2.0","id":"call","method":"tools.call","params":params})
}
fn response(id: u8) -> Value {
    json!({"op":"result","peer":"p","frame":{"jsonrpc":"2.0","id":format!("hive-call-fixture-{id}"),"result":"done"}})
}
fn finish(name: &str, value: Value) -> Value {
    json!({"op":"complete","name":name,"value":value})
}
fn registered() -> Vec<Value> {
    vec![connect("p", Some(("node", 1.0))), update("node", 1.0)]
}

#[test]
fn original_composed_routing_diagnostic_retention_and_resolution() {
    let mut traces = vec![];
    let mut steps = registered();
    steps.extend([invoke("a", tool('a')), response(1)]);
    let mut t = composed("http-uses-exact-connected-peer-and-routing", steps);
    t["expectKinds"] = json!(["invoke-ok"]);
    t["options"]["validateMessages"] = json!(true);
    traces.push(t);
    for failed in [false, true] {
        let mut steps = registered();
        steps.push(invoke("stale", tool('b')));
        steps.push(if failed {json!({"op":"complete","name":"diagnostic","error":{"code":"store_lost","message":"Retain failed.","outcome":"unknown"}})} else {finish("diagnostic",Value::Null)});
        steps.extend([
            invoke("duplicate", tool('b')),
            invoke("current", tool('a')),
            response(1),
            finish("diagnostic", Value::Null),
        ]);
        let mut t = composed(format!("retained-rejection-to-completion-{failed}"), steps);
        t["expectKinds"] = if failed {
            json!(["warning", "invoke-ok"])
        } else {
            json!(["worker", "invoke-ok"])
        };
        traces.push(t);
    }
    compare("server-composition-diagnostics", traces);
}

#[test]
fn original_composed_shared_admission_dispatch_credentials_and_reset() {
    let mut traces = vec![];
    let mut t = composed(
        "http-holds-shared-ws-request-reservation",
        vec![
            connect("c", None),
            invoke("hold", request("hold", false)),
            message("c", request("blocked", false)),
            complete("hold", false, json!(1)),
            json!({"op":"probe","key":"credential","bytes":0}),
        ],
    );
    t["requests"]["perConsumer"] = json!(1);
    t["expectKinds"] = json!(["invoke-ok", "probe"]);
    traces.push(t);
    let replacement = json!([{"principalId":"new","digest":"new-digest"}]);
    for reject in [false, true] {
        let mut steps = registered();
        steps.extend([
            connect("c", None),
            invoke("hold", request("hold", false)),
            json!({"op":"credentials","name":"replace","value":replacement}),
            message("c", request("after", false)),
            complete("hold", false, json!(1)),
        ]);
        steps.push(if reject {json!({"op":"complete","name":"credentials.configure","error":{"code":"refused","message":"Not saved.","outcome":"not_executed"}})}else{finish("credentials.configure",Value::Null)});
        steps.extend([
            complete("after", false, json!(2)),
            invoke("route", tool('a')),
        ]);
        if reject {
            steps.push(response(1));
        }
        let mut t = composed(
            format!("credential-replacement-shares-dispatch-and-routing-{reject}"),
            steps,
        );
        t["dispatch"]["maximum"] = json!(4);
        t["expectKinds"] = if reject {
            json!(["credentials-error", "invoke-ok"])
        } else {
            json!(["credentials-ok", "cache-clear", "terminate"])
        };
        traces.push(t);
    }
    for (transport, digest) in [
        ("http", "credential"),
        ("ws", "bootstrap"),
        ("http", "bootstrap"),
    ] {
        let mut step = invoke("reset", request("reset", false));
        step["context"]["transport"] = json!(transport);
        step["context"]["credentialDigest"] = json!(digest);
        let mut steps = vec![step];
        if transport == "http" && digest == "bootstrap" {
            steps.push(complete("reset", false, json!(true)));
        }
        let mut t = composed(format!("reset-admission-{transport}-{digest}"), steps);
        t["options"]["resetBootstrapCredentialDigest"] = json!("bootstrap");
        traces.push(t);
    }
    compare("server-composition-admission", traces);
}

#[test]
fn original_composed_worker_recovery_events_and_subscription_identity() {
    let mut traces = vec![];
    let mut steps = registered();
    steps.extend([
        connect("c", None),
        invoke("pending-http", tool('a')),
        message("c", tool('a')),
        json!({"op":"failure"}),
        invoke("cleared", tool('a')),
        json!({"op":"failure"}),
        json!({"op":"advance","ms":1000}),
        finish("start", Value::Null),
        finish("binding.diagnostics", json!([])),
        connect("p", Some(("node", 2.0))),
        update("node", 2.0),
        invoke("recovered", tool('a')),
        response(3),
    ]);
    let mut t = composed(
        "failure-clears-shared-http-ws-state-and-coalesces-recovery",
        steps,
    );
    t["expectKinds"] = json!([
        "cache-clear",
        "terminate",
        "invoke-error",
        "storage-start",
        "invoke-ok"
    ]);
    traces.push(t);
    let mut t = composed(
        "worker-events-share-subscription-session-and-immediate-turn",
        vec![
            connect("c", None),
            subscribe("c", json!([]), Some(json!(["objects/a"]))),
            json!({"op":"changes","scopes":["objects/a/child"]}),
            json!({"op":"events","through":4}),
            json!({"op":"events","through":2}),
            json!({"op":"publish"}),
            json!({"op":"publish"}),
        ],
    );
    t["expectKinds"] = json!(["send", "wake"]);
    traces.push(t);
    compare("server-composition-events", traces);
}
