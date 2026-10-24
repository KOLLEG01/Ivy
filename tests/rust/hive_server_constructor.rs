use super::*;
use ivy_hive::{
    browser_push::{self, Sender, VapidDetails, Worker},
    oauth::{self, Parameters},
    packages::{PackageRequest, PackageStorage},
    server_constructor::{Dependencies, Options, Prepared},
    server_lifecycle::{Listener, StorageLifecycle},
    server_options::Error,
    server_rpc::{Reply, Storage},
    server_runtime::Admission,
    store::Credential,
    worker_client::RequestOptions,
    worker_runtime::{Event, WorkerLaunch},
};
use ivy_host_runtime::{native_path, node_path};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU8, Ordering as AtomicOrdering},
};

type Events = Arc<Mutex<Vec<IvyJson>>>;
struct WorkerEffects {
    events: Events,
    delivered: Mutex<bool>,
}
impl WorkerEffects {
    fn post(&self, payload: IvyJson) -> Reply {
        self.events.lock().unwrap().push(obj([
            ("kind", "worker".into()),
            ("payload", payload.clone()),
        ]));
        let value = match payload.get("action").unwrap().as_str().unwrap().as_str() {
            "browserPush.next" => {
                let mut delivered = self.delivered.lock().unwrap();
                let deliveries = if *delivered {
                    vec![]
                } else {
                    vec![json(
                        r#"{"id":"delivery","subscription":{"endpoint":"https://push.invalid"},"notice":{"title":"fixture"}}"#,
                    )]
                };
                *delivered = true;
                obj([
                    ("deliveries", IvyJson::Array(deliveries)),
                    ("hasMore", false.into()),
                ])
            }
            "browserPush.keys" => json(r#"{"publicKey":"public","privateKey":"private"}"#),
            "package.catalog" => {
                json(r#"{"catalog":{"schemaVersion":1,"revision":0,"packages":[]}}"#)
            }
            _ => IvyJson::Null,
        };
        Box::pin(async move { Ok(value) })
    }
}
impl Storage for WorkerEffects {
    fn request(&self, payload: IvyJson, _: RequestOptions) -> Reply {
        self.post(payload)
    }
}
impl Worker for WorkerEffects {
    fn request(&self, payload: IvyJson) -> browser_push::RequestFuture {
        self.post(payload)
    }
}
impl PackageStorage for WorkerEffects {
    fn request(&self, payload: IvyJson) -> PackageRequest {
        self.post(payload)
    }
}
impl StorageLifecycle for WorkerEffects {
    fn start(&self) -> Reply<()> {
        panic!("Constructor must not start storage")
    }
    fn close(&self) -> Reply<()> {
        panic!("Constructor must not close storage")
    }
}
struct Effects(Events);
impl Sender for Effects {
    fn send(
        &self,
        subscription: IvyJson,
        payload: String,
        details: VapidDetails,
    ) -> browser_push::SendFuture {
        let options = obj([
            ("vapidDetails", obj([
                ("publicKey", details.public_key.into()),
                ("privateKey", details.private_key.into()),
                ("subject", details.subject.into()),
            ])),
            ("TTL", IvyJson::Number(3600.0)),
            ("timeout", IvyJson::Number(10000.0)),
        ]);
        self.0.lock().unwrap().push(obj([
            ("kind", "send".into()),
            ("subscription", subscription),
            ("payload", payload.into()),
            ("options", options),
        ]));
        Box::pin(async { Ok(201) })
    }
}
impl Listener for Effects {
    fn listen(&self, _: f64, _: JsString) -> Reply<()> {
        panic!("Constructor must not bind a listener")
    }
    fn address(&self) -> IvyResult<IvyJson> {
        panic!("Constructor must not inspect a listener")
    }
    fn close_websockets(&self) -> IvyResult<()> {
        panic!("Constructor must not close sockets")
    }
    fn close_connections(&self) -> IvyResult<()> {
        panic!("Constructor must not close connections")
    }
    fn close(&self) -> Reply<()> {
        panic!("Constructor must not close a listener")
    }
}
impl Runtime for Effects {
    fn parse_date(&self, _: &JsString) -> f64 {
        panic!("No diagnostic dates in constructor fixture")
    }
    fn iso_date(&self, _: f64) -> IvyResult<JsString> {
        panic!("No diagnostic dates in constructor fixture")
    }
    fn compare_contract_keys(&self, _: &JsString, _: &JsString) -> Ordering {
        panic!("No locale effects in constructor fixture")
    }
}
struct Entropy(AtomicU8);
impl oauth::Entropy for Entropy {
    fn bytes(&self, length: usize) -> IvyResult<Vec<u8>> {
        Ok(vec![
            self.0.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            length
        ])
    }
}
fn dependencies(events: Events) -> Dependencies {
    let effects = Arc::new(Effects(events));
    Dependencies {
        sender: effects.clone(),
        redirects: Arc::new(oauth::UrlRedirects),
        entropy: Arc::new(Entropy(AtomicU8::new(0))),
        listener: effects.clone(),
        now: Arc::new(|| 100000.0),
        dates: effects,
        ids: Arc::new(|| panic!("Constructor must not allocate request IDs")),
        provider_error: Arc::new(|parts| {
            let mut error =
                IvyError::new_js(parts.code, parts.message).with_provider_outcome(parts.outcome);
            if let Some(details) = parts.details {
                error = error.with_details(details);
            }
            error
        }),
        immediate: Arc::new(|_| panic!("Constructor must not publish events")),
        unhandled: Arc::new(|_, error| panic!("Unexpected callback: {error:?}")),
        warn: Arc::new(|_| panic!("Constructor must not emit warnings")),
        observe_mcp_call: None,
    }
}
fn string(value: &IvyJson, key: &str) -> JsString {
    value.get(key).unwrap().as_js_string().unwrap().clone()
}
fn options(input: &IvyJson) -> Options {
    Options::from_json(input.clone()).unwrap()
}
fn parameters(entries: impl IntoIterator<Item = (&'static str, JsString)>) -> Parameters {
    Parameters::new(entries.into_iter().map(|(k, v)| (k.into(), v)))
}
async fn execute(input: &IvyJson) -> Result<IvyJson, Error> {
    let prepared = Prepared::new(options(input))?;
    let worker_options = prepared.worker_options().clone();
    let ui_root = prepared.ui_root().clone();
    let events = Events::default();
    let worker = Arc::new(WorkerEffects {
        events: events.clone(),
        delivered: Mutex::new(false),
    });
    let server = prepared.bind(
        worker.clone(),
        dependencies(events.clone()),
        Admission::default(),
    )?;
    let callback = server.network.lifecycle.event_sink();
    let weak = Arc::downgrade(&worker);
    let snapshot = obj([
        ("href", IvyJson::String(server.base.href.clone())),
        ("origin", IvyJson::String(server.base.origin.clone())),
        ("basePath", IvyJson::String(server.base.base_path.clone())),
        ("uiRoot", IvyJson::String(ui_root.clone())),
        ("workerOptions", worker_options),
        (
            "packages",
            server.packages.as_ref().map_or(IvyJson::Null, |v| {
                obj([("root", IvyJson::String(node_path::from_native(v.root())))])
            }),
        ),
        (
            "metadata",
            server
                .oauth
                .authorization_server_metadata(&server.base.base_path)?,
        ),
        ("protected", server.oauth.protected_resource_metadata(None)?),
    ]);
    assert_eq!(server.ui_files.root, node_path::to_native(&ui_root));
    assert_eq!(
        server.network.lifecycle.credentials(),
        input
            .get("credentials")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|v| Credential {
                principal_id: string(v, "principalId"),
                digest: string(v, "digest"),
            })
            .collect::<Vec<_>>()
    );
    assert!(
        events.lock().unwrap().is_empty(),
        "Constructor started an effect"
    );
    server.browser_push.wake();
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    server.browser_push.close().await;
    if let Some(packages) = &server.packages {
        packages
            .catalog_snapshot(&IvyJson::Number(0.0), true)
            .await?;
    }
    server.oauth.initialize(&oauth::Secrets {
        signing_key: "11".repeat(32).into(),
        reference_key: "22".repeat(32).into(),
    })?;
    let registered = server.oauth.register(&json(
        r#"{"redirect_uris":["https://client.invalid/callback"]}"#,
    ))?;
    let client = string(&registered, "client_id");
    let verifier = "v".repeat(43);
    let challenge = {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use sha2::{Digest, Sha256};
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    };
    let begun = server.oauth.begin(&parameters([
        ("response_type", "code".into()),
        ("client_id", client.clone()),
        ("redirect_uri", "https://client.invalid/callback".into()),
        ("state", "fixture".into()),
        ("code_challenge", challenge.into()),
        ("code_challenge_method", "S256".into()),
        ("resource", server.oauth.resource().clone()),
    ]))?;
    let redirect = server
        .oauth
        .authorize(&string(&begun, "transaction"), &"credential".into())?;
    let code = Parameters::parse_form(&redirect_query(&redirect))
        .get("code")
        .unwrap()
        .clone();
    let token = server
        .oauth
        .token(&parameters([
            ("grant_type", "authorization_code".into()),
            ("client_id", client.clone()),
            ("resource", server.oauth.resource().clone()),
            ("redirect_uri", "https://client.invalid/callback".into()),
            ("code", code),
            ("code_verifier", verifier.into()),
        ]))
        .await?;
    let refreshed = server
        .oauth
        .token(&parameters([
            ("grant_type", "refresh_token".into()),
            ("client_id", client),
            ("resource", server.oauth.resource().clone()),
            ("refresh_token", string(&token, "refresh_token")),
        ]))
        .await?;
    let result = obj([
        ("snapshot", snapshot),
        ("events", IvyJson::Array(events.lock().unwrap().clone())),
        ("registered", registered),
        ("begun", begun),
        ("redirect", IvyJson::String(redirect)),
        ("token", token),
        ("refreshed", refreshed),
    ]);
    drop(server);
    drop(worker);
    assert!(
        weak.upgrade().is_none(),
        "A stored worker callback retained its owner"
    );
    callback(Event::Notification(
        ivy_hive::worker_protocol::Notification::Changes(vec![]),
    ));
    Ok(result)
}
fn redirect_query(value: &JsString) -> JsString {
    let at = value.0.iter().position(|c| *c == b'?' as u16).unwrap();
    JsString(value.0[at + 1..].to_vec())
}
#[test]
fn original_constructor_paths_options_and_bound_business_owners() {
    let output = Command::new(std::env::var_os("IVY_TEST_NODE").unwrap_or_else(|| "node".into()))
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .arg("tests/fixtures/hive-server-constructor-oracle.mjs")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = json(std::str::from_utf8(&output.stdout).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let rows = original.get("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 25);
    let mut observations = vec![];
    for row in rows {
        let input = row.get("input").unwrap().get("options").unwrap();
        let native = match runtime.block_on(execute(input)) {
            Ok(value) => obj([("value", value)]),
            Err(Error::Ivy(error)) => obj([("error", error.to_wire())]),
            Err(Error::UrlConstructor { input }) => {
                obj([("urlConstructorInput", IvyJson::String(input))])
            }
        };
        observations.push(obj([("original", row.clone()), ("native", native.clone())]));
        if let Some(path) = std::env::var_os("IVY_HIVE_SERVER_BINDINGS_CORPUS") {
            std::fs::write(path, encode(&IvyJson::Array(observations.clone()))).unwrap();
        }
        assert_eq!(
            &native,
            row.get("expected").unwrap(),
            "{}",
            encode(row.get("input").unwrap())
        );
    }
}
fn launch() -> WorkerLaunch {
    WorkerLaunch {
        root: PathBuf::from("constructor-unused-root"),
        executable: PathBuf::from("constructor-unused-executable"),
        args: vec!["--storage".into()],
        options: IvyJson::Null,
        job_launcher: Some("constructor-unused-job".into()),
        environment: BTreeMap::from([("CONSTRUCTOR_FIXTURE".into(), "retained".into())]),
    }
}
#[test]
fn native_constructor_is_inert_and_does_not_retain_lifecycle() {
    let source = json(
        r#"{"filename":"state/hive.sqlite","publicBaseUrl":"https://example.test/ivy/","version":"1.2.3","buildId":"build:test","credentials":[{"principalId":"a","digest":"credential"}],"fixtureOption":{"value":"retained"},"observeMcpCall":null,"sendBrowserPush":false}"#,
    );
    let mut cases = vec![];
    let mut add = |name: &str, filename: JsString, explicit: Option<IvyJson>, path: PathBuf| {
        let public = explicit
            .as_ref()
            .and_then(IvyJson::as_js_string)
            .cloned()
            .unwrap_or_else(|| native_path::from_native(&path));
        let mut input = source.clone();
        input.insert("filename", filename.into());
        if let Some(explicit) = explicit {
            input.insert("uiRoot", explicit);
        }
        cases.push((name.to_owned(), input, public, path));
    };
    for (name, filename, parent) in [
        ("relative-filename", "state/hive.sqlite", "state"),
        ("bare-filename", "hive.sqlite", ""),
        ("empty-filename", "", ""),
        ("dot-filename", ".", ""),
        ("parent-filename", "..", ""),
        ("memory-filename", ":memory:", ""),
        ("lexical-parent", "state/../hive.sqlite", ""),
        ("parent-as-filename", "state/..", "state"),
        ("unicode-filename", "漢字 space/hive.sqlite", "漢字 space"),
    ] {
        add(
            name,
            filename.into(),
            None,
            PathBuf::from(parent).join("ui-releases"),
        );
    }
    let cwd = std::env::current_dir().unwrap();
    let absolute = cwd.join("constructor-data");
    add(
        "absolute-filename",
        native_path::from_native(&absolute.join("hive.sqlite")),
        None,
        absolute.join("ui-releases"),
    );
    let root = cwd.ancestors().last().unwrap();
    add(
        "root-filename",
        native_path::from_native(root),
        None,
        root.join("ui-releases"),
    );
    for (name, path) in [
        ("explicit-empty", PathBuf::new()),
        ("explicit-lexical-text", PathBuf::from("ui/../custom. ")),
        ("explicit-whitespace", PathBuf::from("  ")),
        ("explicit-absolute", cwd.join("explicit-ui")),
    ] {
        add(
            name,
            "ignored/hive.sqlite".into(),
            Some(native_path::from_native(&path).into()),
            path,
        );
    }
    add(
        "null-default",
        "state/hive.sqlite".into(),
        Some(IvyJson::Null),
        PathBuf::from("state").join("ui-releases"),
    );
    #[cfg(windows)]
    {
        for (name, filename, path) in [
            ("drive-prefix-only", r"C:", PathBuf::from("ui-releases")),
            (
                "drive-relative",
                r"C:hive.sqlite",
                PathBuf::from("C:").join("ui-releases"),
            ),
            (
                "rooted-filename",
                r"\hive.sqlite",
                PathBuf::from(r"\").join("ui-releases"),
            ),
            (
                "unc-root",
                r"\\server\share\",
                PathBuf::from(r"\\server\share\").join("ui-releases"),
            ),
            (
                "verbatim-parent",
                r"\\?\C:\data\..\store\hive.sqlite",
                PathBuf::from(r"\\?\C:\store").join("ui-releases"),
            ),
            (
                "trailing-parent",
                r"C:\data. \hive.sqlite",
                PathBuf::from(r"C:\data. ").join("ui-releases"),
            ),
        ] {
            add(name, filename.into(), None, path);
        }
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        for (name, surrogate) in [("high", 0xd800), ("low", 0xdfff)] {
            let mut units: Vec<_> = "data".encode_utf16().collect();
            units.push(surrogate);
            let parent = PathBuf::from("state").join(OsString::from_wide(&units));
            add(
                &format!("opaque-{name}-filename"),
                native_path::from_native(&parent.join("hive.sqlite")),
                None,
                parent.join("ui-releases"),
            );
            add(
                &format!("opaque-{name}-explicit"),
                "ignored/hive.sqlite".into(),
                Some(native_path::from_native(&parent).into()),
                parent,
            );
        }
    }
    assert_eq!(cases.len(), if cfg!(windows) { 26 } else { 16 });
    for (name, input, public, path) in cases {
        let prepared = Prepared::new(options(&input)).unwrap();
        assert_eq!(prepared.ui_root(), &public, "{name}: exact public root");
        let mut expected = input;
        expected.insert("uiRoot", public.into());
        expected.remove("observeMcpCall");
        expected.remove("sendBrowserPush");
        assert_eq!(
            prepared.worker_options(),
            &expected,
            "{name}: exact worker data"
        );
        let original = launch();
        let actual = prepared.launch_options(original.clone());
        assert_eq!(actual.options, expected, "{name}");
        assert_eq!(actual.root, original.root);
        assert_eq!(actual.executable, original.executable);
        assert_eq!(actual.args, original.args);
        assert_eq!(actual.job_launcher, original.job_launcher);
        assert_eq!(actual.environment, original.environment);
        let events = Events::default();
        let server = prepared
            .native(original, dependencies(events.clone()), Admission::default())
            .unwrap();
        assert_eq!(
            server.ui_files.root.as_os_str(),
            path.as_os_str(),
            "{name}: exact private native path"
        );
        assert_eq!(
            server.worker.lifecycle(),
            ivy_hive::worker_client::Lifecycle::Stopped
        );
        assert_eq!(server.worker.pending_count(), 0);
        assert!(
            events.lock().unwrap().is_empty(),
            "{name}: constructor effect"
        );
        let weak = Arc::downgrade(&server.worker);
        let callback = server.network.lifecycle.event_sink();
        drop(server);
        assert!(weak.upgrade().is_none(), "{name}: retained lifecycle owner");
        callback(Event::Notification(
            ivy_hive::worker_protocol::Notification::Changes(vec![]),
        ));
    }
}

#[path = "hive_server_authentication.rs"]
mod authentication;

#[path = "hive_server_http.rs"]
mod http;

#[path = "hive_mcp_owner.rs"]
mod mcp_owner;
