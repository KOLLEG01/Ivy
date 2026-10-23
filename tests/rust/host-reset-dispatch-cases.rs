use super::*;
use crate::host_config::HostContext;
use ivy_sdk::clock::SystemClock;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::Mutex,
};

static CORPUS: Mutex<()> = Mutex::new(());
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ivy-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn oracle_script(input: &J, script: &str) -> J {
    let mut child = Command::new(std::env::var_os("IVY_RESTORE_NODE").unwrap())
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
        ])
        .arg(root().join("tests/fixtures").join(script))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn oracle(input: &J) -> J {
    oracle_script(input, "host-reset-dispatch-oracle.mjs")
}
fn wrap(result: IvyResult<J>) -> J {
    match result {
        Ok(value) => J::object([("ok", true.into()), ("value", value)]),
        Err(error) => J::object([("ok", false.into()), ("error", error.to_wire())]),
    }
}
fn compare(group: &str, setup: J, actual: J, expected: J) {
    if let Some(path) = std::env::var_os("IVY_HOST_RESET_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(
            file,
            "{}",
            json::canonical(
                &J::object([
                    ("group", group.into()),
                    ("setup", setup),
                    ("actual", actual.clone()),
                    ("expected", expected.clone()),
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_eq!(
        json::canonical(&actual, usize::MAX).unwrap(),
        json::canonical(expected.get("result").unwrap(), usize::MAX).unwrap(),
        "{group}"
    );
}
fn phase(name: &str) -> ResetHostPhase {
    match name {
        "prepare" => ResetHostPhase::Prepare,
        "complete" => ResetHostPhase::Complete,
        "bootstrap" => ResetHostPhase::Bootstrap,
        "release" => ResetHostPhase::Release,
        "confirm" => ResetHostPhase::Confirm,
        _ => panic!("Owned phase only"),
    }
}
fn args(row: &J) -> ResetPhaseArguments {
    let get = |key| row.get(key).and_then(J::as_js_string).cloned();
    ResetPhaseArguments {
        reset_id: get("resetId"),
        runtime_epoch: get("runtimeEpoch"),
        scope_roots: get("scopeRoots"),
        publisher_digest: get("publisherDigest"),
    }
}
fn authority(row: &J) -> CoordinatedReset {
    CoordinatedReset {
        reset_id: field(row, "resetId").unwrap(),
        runtime_epoch: field(row, "runtimeEpoch").unwrap(),
        scope_roots: field(row, "scopeRoots").unwrap(),
        publisher_digest: field(row, "publisherDigest").unwrap(),
    }
}
fn row(kind: &str) -> J {
    J::object([
        ("kind", kind.into()),
        ("resetId", "retained-reset ' \" $(literal) é 🚀".into()),
        ("runtimeEpoch", "retained-epoch".into()),
        ("scopeRoots", "WyJhIiwiYiJd".into()),
        (
            "publisherDigest",
            format!("sha256:{}", "d".repeat(64)).into(),
        ),
    ])
}
fn plan() -> J {
    J::object([
        ("hostId", "fixture".into()),
        ("scopeRootIds", J::Array(vec!["local".into()])),
        ("retainedAuthority", true.into()),
    ])
}
fn pure(cases: Vec<J>) {
    let setup = J::object([("kind", "pure".into()), ("cases", J::Array(cases.clone()))]);
    let expected = oracle(&setup);
    let actual = cases
        .iter()
        .map(|row| {
            wrap(match row.get("kind").unwrap().as_str().unwrap().as_str() {
                "encode" => encode_scope_roots(
                    &row.get("ids")
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_js_string().unwrap().clone())
                        .collect::<Vec<_>>(),
                )
                .map(J::String),
                "decode" => decode_scope_roots(row.get("value").unwrap().as_js_string().unwrap())
                    .map(|v| J::Array(v.into_iter().map(J::String).collect())),
                "complexity" => {
                    let encoded = URL_SAFE_NO_PAD.encode(
                        json::stringify(&J::Array(vec!["a".into(); 400_000]), usize::MAX).unwrap(),
                    );
                    decode_scope_roots(&encoded.into())
                        .map(|v| J::Array(v.into_iter().map(J::String).collect()))
                }
                "admit" => admit_plan(
                    row.get("plan").unwrap().clone(),
                    phase(&row.get("phase").unwrap().as_str().unwrap()),
                    &args(row),
                ),
                "argv" => Ok(J::Array(
                    reset_phase_arguments(
                        row.get("phase").unwrap().as_js_string().unwrap(),
                        &authority(row),
                    )
                    .into_iter()
                    .map(J::String)
                    .collect(),
                )),
                _ => panic!("Unknown pure boundary"),
            })
        })
        .collect();
    compare("pure-dispatch", setup, J::Array(actual), expected);
}

#[test]
fn scope_codec_matches_node_base64_json_and_utf16_order() {
    let mut cases = Vec::new();
    for ids in [
        vec![],
        vec!["b".into(), "a".into()],
        vec!["é".into(), "🚀".into()],
        vec![JsString(vec![0xd800]), JsString(vec![0xdc00])],
    ] {
        cases.push(J::object([
            ("kind", "encode".into()),
            ("ids", J::Array(ids.into_iter().map(J::String).collect())),
        ]));
    }
    for text in [
        "[]",
        "[\"a\"]",
        "[\"a\",\"b\"]",
        "[\"b\",\"a\"]",
        "[\"a\",\"a\"]",
        "[\"\"]",
        "[1]",
        "[null]",
        "[true]",
        "[{}]",
        "[[\"a\"]]",
        "null",
        "{}",
        "true",
        "0",
        "[\"\\ud800\",\"\\udc00\"]",
        "[\"🚀\",\"中\"]",
        "[\"中\",\"🚀\"]",
        " [ \"a\" ] \n",
        "[\"\\u0000\"]",
        "\u{feff}[]",
        "[\"a\",]",
        "[\"a\"]garbage",
    ] {
        cases.push(J::object([
            ("kind", "decode".into()),
            ("value", URL_SAFE_NO_PAD.encode(text).into()),
        ]));
    }
    for value in [
        "",
        "A",
        "W10",
        "W11",
        "W10=ignored",
        "W1\r\n0",
        "W1!0",
        "W1\u{010d}0",
        "\u{0157}\u{0131}\u{0130}",
        "W1\0\u{100}0",
        "W10\u{ff1d}junk",
        "W10====",
    ] {
        cases.push(J::object([
            ("kind", "decode".into()),
            ("value", value.into()),
        ]));
    }
    for unit in [0xd800, 0xd83d, 0xde00, 0x013d] {
        cases.push(J::object([
            ("kind", "decode".into()),
            ("value", J::String(JsString(vec![87, 49, unit, 48]))),
        ]));
    }
    for (bytes, standard) in [
        (b"[\"\xff\"]".as_slice(), false),
        (b"[\"\xf0\x9f\"]", false),
        (b"[\"\xed\xa0\x80\"]", true),
        (b"\xef\xbb\xbf[]", false),
    ] {
        let value = if standard {
            base64::engine::general_purpose::STANDARD.encode(bytes)
        } else {
            URL_SAFE_NO_PAD.encode(bytes)
        };
        cases.push(J::object([
            ("kind", "decode".into()),
            ("value", value.into()),
        ]));
    }
    for id in [
        "x".repeat(256),
        "x".repeat(257),
        "🚀".repeat(128),
        "🚀".repeat(129),
    ] {
        cases.push(J::object([
            ("kind", "decode".into()),
            (
                "value",
                J::String(encode_scope_roots(&[id.into()]).unwrap()),
            ),
        ]));
    }
    cases.push(J::object([("kind", "complexity".into())]));
    pure(cases);
}

#[test]
fn admission_preserves_identity_order_and_only_prepare_replaces_roots() {
    let mut cases = Vec::new();
    for name in ["prepare", "complete", "bootstrap", "release", "confirm"] {
        for key in [
            None,
            Some("resetId"),
            Some("runtimeEpoch"),
            Some("scopeRoots"),
            Some("publisherDigest"),
        ] {
            let mut value = row("admit");
            value.insert("phase", name.into());
            value.insert("plan", plan());
            if let Some(key) = key {
                value.insert(key, "".into());
            }
            cases.push(value);
        }
    }
    for (roots, digest) in [
        ("not-json", "bad"),
        ("W10", "bad"),
        ("W10", " "),
        ("WyJiIiwiYSJd", "valid-unused"),
        ("W10=ignored", "valid-unused"),
    ] {
        let mut value = row("admit");
        value.insert("phase", "prepare".into());
        value.insert("plan", plan());
        value.insert("scopeRoots", roots.into());
        value.insert("publisherDigest", digest.into());
        cases.push(value);
    }
    let mut missing = row("admit");
    missing.insert("phase", "prepare".into());
    missing.insert("plan", plan());
    missing.remove("resetId");
    missing.insert("scopeRoots", "invalid".into());
    cases.push(missing);
    pure(cases);
}

#[test]
fn coordinated_argv_matches_the_original_invoke_expression() {
    let mut cases = Vec::new();
    for name in [
        "prepare",
        "clear",
        "complete",
        "bootstrap",
        "release",
        "confirm",
    ] {
        let mut value = row("argv");
        value.insert("phase", name.into());
        cases.push(value);
    }
    let mut empty = row("argv");
    empty.insert("phase", "prepare".into());
    empty.insert("resetId", "".into());
    empty.insert("scopeRoots", " ".into());
    cases.push(empty);
    pure(cases);
}

async fn native(distribution: PathBuf) -> (ResetHost, NodeRuntime) {
    let node = NodeRuntime::discover(Some(&PathBuf::from(
        std::env::var_os("IVY_RESTORE_NODE").unwrap(),
    )))
    .await
    .unwrap();
    let host = ResetHost::new(
        distribution,
        PathBuf::from(std::env::var_os("IVY_BACKUP_JOB").unwrap()),
        Arc::new(SystemClock),
    )
    .unwrap();
    (host, node)
}
fn storage(comparisons: &J) -> Arc<layout::StorageContext> {
    let context = HostContext::current().unwrap();
    let pairs = comparisons.as_array().unwrap().to_vec();
    Arc::new(layout::StorageContext {
        paths: context.paths,
        account_home: context.account_home,
        compare: Arc::new(move |a, b| {
            let pair = pairs
                .iter()
                .find(|v| {
                    let p = v.as_array().unwrap();
                    p[0].as_js_string() == Some(a) && p[1].as_js_string() == Some(b)
                })
                .expect("Only actual captured Node locale pairs are qualified");
            let n = pair.as_array().unwrap()[2].as_f64().unwrap();
            if n < 0.0 {
                std::cmp::Ordering::Less
            } else if n > 0.0 {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        }),
    })
}
fn setup(fixture: &Fixture) -> J {
    oracle_script(
        &J::object([
            ("kind", "setup".into()),
            ("case", "valid".into()),
            ("fixtureRoot", J::String(node_path::from_native(&fixture.0))),
            (
                "root",
                J::String(node_path::from_native(&fixture.0.join("plan"))),
            ),
        ]),
        "host-reset-plan-oracle.mjs",
    )
}

#[cfg(windows)]
#[tokio::test]
async fn local_dispatch_plans_before_admission_and_retains_real_denial() {
    let fixture = Fixture::new();
    let original = setup(&fixture);
    let distribution = PathBuf::from(field(&original, "distribution").unwrap().to_utf8().unwrap());
    let (host, node) = native(distribution).await;
    assert!(
        host.require_administrator().await.is_err(),
        "This disposable check requires the actual non-elevated token"
    );
    for (name, case) in [
        ("prepare", "missing-reset"),
        ("prepare", "missing-scope"),
        ("prepare", "bad-roots"),
        ("prepare", "bad-digest"),
        ("prepare", "actual-admission"),
        ("complete", "actual-admission"),
        ("bootstrap", "actual-admission"),
        ("release", "actual-admission"),
        ("confirm", "actual-admission"),
        ("confirm", "planner-first"),
        ("confirm", "local-coordinator"),
    ] {
        let mut input = row("full");
        input.insert("fixtureRoot", J::String(node_path::from_native(&fixture.0)));
        input.insert("phase", name.into());
        input.insert("case", case.into());
        for key in ["config", "configPath", "distribution"] {
            input.insert(key, original.get(key).unwrap().clone());
        }
        match case {
            "missing-reset" => {
                input.remove("resetId");
            }
            "missing-scope" => {
                input.remove("scopeRoots");
            }
            "bad-roots" => {
                input.insert("scopeRoots", "garbage".into());
            }
            "bad-digest" => {
                input.insert("publisherDigest", "invalid".into());
            }
            "planner-first" => {
                let mut config = input.remove("config").unwrap();
                config.remove("deployment");
                input.insert("config", config);
                input.remove("resetId");
            }
            _ => (),
        }
        if case == "local-coordinator" {
            input.insert("kind", "invoke".into());
            input.insert(
                "host",
                J::object([(
                    "hostId",
                    original
                        .get("config")
                        .unwrap()
                        .get("hostId")
                        .unwrap()
                        .clone(),
                )]),
            );
        }
        let expected = oracle(&input);
        let actual = if case == "local-coordinator" {
            wrap(
                host.invoke_phase(
                    input.get("config").unwrap().clone(),
                    field(&input, "configPath").unwrap(),
                    input.get("host").unwrap().clone(),
                    phase(name),
                    authority(&input),
                    node.clone(),
                    storage(expected.get("comparisons").unwrap()),
                )
                .await
                .map(|v| v.unwrap()),
            )
        } else {
            wrap(
                host.run_phase(
                    input.get("config").unwrap().clone(),
                    field(&input, "configPath").unwrap(),
                    phase(name),
                    args(&input),
                    node.clone(),
                    storage(expected.get("comparisons").unwrap()),
                )
                .await,
            )
        };
        compare("local-phase-dispatch", input, actual, expected);
    }
    let config = original.get("config").unwrap();
    assert!(!reset_record_path(config).unwrap().exists());
    assert!(
        !crate::runtime_maintenance::runtime_reset_marker(config)
            .unwrap()
            .exists()
    );
}

#[tokio::test]
async fn coordinated_remote_routing_uses_existing_owned_process_path() {
    let fixture = Fixture::new();
    let original = setup(&fixture);
    let (host, node) = native(root()).await;
    let mut config = original.get("config").unwrap().clone();
    let mut executables = config.remove("executables").unwrap();
    executables.insert(
        "ssh",
        J::String(node_path::from_native(&PathBuf::from(
            std::env::var_os("IVY_RESTORE_NODE").unwrap(),
        ))),
    );
    config.insert("executables", executables);
    // Deliberately invalid for local planning: remote dispatch must not invoke it.
    config.remove("deployment");
    let target = J::object([
        ("hostId", "remote-fixture".into()),
        ("ssh", "--no-warnings".into()),
        (
            "cliPath",
            J::String(node_path::from_native(
                &root().join("tests/fixtures/host-reset-ssh-fixture.mjs"),
            )),
        ),
        (
            "configPath",
            J::String(node_path::from_native(
                &fixture.0.join("literal config.json"),
            )),
        ),
    ]);
    for name in ["prepare", "complete", "bootstrap", "release", "confirm"] {
        let mut input = row("invoke");
        input.insert("fixtureRoot", J::String(node_path::from_native(&fixture.0)));
        input.insert("phase", name.into());
        input.insert("config", config.clone());
        input.insert("host", target.clone());
        input.insert("configPath", original.get("configPath").unwrap().clone());
        input.insert("distribution", J::String(node_path::from_native(&root())));
        let expected = oracle(&input);
        let actual = wrap(
            host.invoke_phase(
                config.clone(),
                field(&input, "configPath").unwrap(),
                target.clone(),
                phase(name),
                authority(&input),
                node.clone(),
                storage(expected.get("comparisons").unwrap()),
            )
            .await
            .map(|v| v.unwrap()),
        );
        compare("coordinated-remote-dispatch", input, actual, expected);
    }
}
