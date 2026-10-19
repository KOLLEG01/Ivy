use ivy_agent_manager::environment_manager::{
    EnvironmentManagerSettings, EnvironmentStatus, ManagedEnvironment,
};
use ivy_contracts::{
    IvyError, IvyJson, JsString,
    hash::digest,
    json::{canonical, parse},
};
use ivy_host_runtime::node_path;
use ivy_sdk::rpc::{RequestFuture, RequestOptions, RpcClient};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, watch};
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-environment-owner-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn flag(value: &IvyJson, key: &str) -> bool {
    value.get(key) == Some(&IvyJson::Bool(true))
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap()
    );
}
fn original() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let output = Command::new("node")
            .arg("tests/fixtures/agent-environment-manager-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
    })
}
fn defaults(conflict: bool) -> IvyJson {
    IvyJson::object([
        (
            "instructions",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("hostId", IvyJson::Null),
                ("enabled", true.into()),
                ("includeLocal", true.into()),
                ("text", "Fixture instructions".into()),
            ]),
        ),
        (
            "mcp",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("hostId", IvyJson::Null),
                ("enabled", true.into()),
                (
                    "servers",
                    IvyJson::Array(vec![IvyJson::object([
                        ("name", "fixture".into()),
                        (
                            "url",
                            if conflict {
                                "https://other.example/mcp"
                            } else {
                                "https://fixture.example/mcp"
                            }
                            .into(),
                        ),
                        ("enabled", true.into()),
                        ("authentication", "agent-manager".into()),
                        ("startupTimeoutSeconds", IvyJson::Number(10.0)),
                        ("toolTimeoutSeconds", IvyJson::Number(30.0)),
                    ])]),
                ),
            ]),
        ),
        (
            "skills",
            IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("hostId", IvyJson::Null),
                ("enabled", true.into()),
                (
                    "files",
                    IvyJson::Array(vec![IvyJson::object([
                        ("path", "fixture/SKILL.md".into()),
                        ("content", "# Fixture\n".into()),
                    ])]),
                ),
            ]),
        ),
    ])
}
fn create(base: &Path, definition: &IvyJson) -> (ManagedEnvironment, Arc<Client>) {
    let home = base.join("home");
    let data = base.join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir(&data).unwrap();
    fs::create_dir(base.join("other-home")).unwrap();
    fs::write(home.join("AGENTS.md"), "# Local\nFixture local text\n").unwrap();
    let mut settings = IvyJson::object([] as [(&str, IvyJson); 0]);
    if flag(definition, "disabled") {
        settings.insert("patcherEnabled", false.into());
    }
    if flag(definition, "claude") {
        settings.insert(
            "appServer",
            IvyJson::object([("mode", "claude-adapter".into())]),
        );
    }
    if flag(definition, "explicit") {
        settings.insert(
            "skillsRoot",
            IvyJson::String(node_path::from_native(&base.join("explicit-skills"))),
        );
    }
    let skill = if flag(definition, "explicit") {
        base.join("explicit-skills")
    } else if flag(definition, "claude") {
        data.join("claude-config/skills")
    } else {
        base.join(".agents/skills")
    };
    let mcp = if flag(definition, "claude") {
        data.join("claude-mcp.json")
    } else {
        home.join("config.toml")
    };
    let environment = ManagedEnvironment::with_clock(
        EnvironmentManagerSettings {
            settings,
            data_root: node_path::from_native(&data),
            host_id: "fixture".into(),
            hive_url: "https://fixture.example".into(),
            credential: "fixture-only".into(),
            defaults: defaults(flag(definition, "conflict")),
            // This corpus has a modern hint and one skill entry. Any attempted locale
            // comparison must fail rather than qualify an ordinal production fallback.
            english_hint_comparator: Arc::new(|_, _| {
                panic!("No production English comparator in this fixture")
            }),
            skills_comparator: Arc::new(|_, _| {
                panic!("No production default comparator in this fixture")
            }),
        },
        Arc::new(|| 1234.0),
    );
    let client = Arc::new(Client {
        calls: Mutex::new(vec![]),
        first: Mutex::new(None),
        paths: [
            home.join("AGENTS.override.md"),
            mcp,
            skill.join("fixture/SKILL.md"),
        ],
        unavailable: flag(definition, "unavailable"),
        gate: None,
        entered: None,
    });
    (environment, client)
}
struct Client {
    calls: Mutex<Vec<IvyJson>>,
    first: Mutex<Option<IvyJson>>,
    paths: [PathBuf; 3],
    unavailable: bool,
    gate: Option<watch::Receiver<bool>>,
    entered: Option<Arc<Notify>>,
}
impl RpcClient for Client {
    fn request<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        _: RequestOptions,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(IvyJson::object([
                ("method", method.into()),
                ("params", params),
            ]));
            {
                let mut first = self.first.lock().unwrap();
                if first.is_none() {
                    *first = Some(IvyJson::object([
                        ("instructions", self.paths[0].exists().into()),
                        ("mcp", self.paths[1].exists().into()),
                        ("skill", self.paths[2].exists().into()),
                    ]));
                }
            }
            if let Some(entered) = &self.entered {
                entered.notify_one();
            }
            if let Some(gate) = &self.gate {
                let mut gate = gate.clone();
                while !*gate.borrow_and_update() {
                    gate.changed().await.unwrap();
                }
            }
            if self.unavailable {
                return Err(IvyError::new("service_unavailable", "fixture unavailable"));
            }
            match method {
                "openai.getConfiguration" => Ok(IvyJson::object([
                    ("revision", IvyJson::Number(0.0)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", IvyJson::Null),
                        ]),
                    ),
                ])),
                "discovery.instructions" => Ok(IvyJson::object([(
                    "instructions",
                    "Fixture guidance".into(),
                )])),
                "objects.stat" => Err(IvyError::new("not_found", "fixture missing")),
                _ => panic!("unexpected method {method}"),
            }
        })
    }
}
fn normalize(value: &IvyJson, base: &Path) -> IvyJson {
    IvyJson::object(
        value
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, _)| name != &JsString::from("observedAt"))
            .map(|(name, value)| {
                if name == &JsString::from("home") || name == &JsString::from("target") {
                    let path = node_path::filesystem_path(value.as_js_string().unwrap());
                    (
                        name.clone(),
                        path.strip_prefix(base)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/")
                            .into(),
                    )
                } else {
                    (name.clone(), value.clone())
                }
            }),
    )
}
fn file(path: &Path) -> IvyJson {
    match fs::read(path) {
        Ok(bytes) => IvyJson::object([
            ("hash", digest(&bytes).into()),
            ("bytes", IvyJson::Number(bytes.len() as f64)),
            (
                "text",
                if bytes.len() <= 8192 {
                    String::from_utf8_lossy(&bytes).to_string().into()
                } else {
                    IvyJson::Null
                },
            ),
        ]),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => IvyJson::Null,
        Err(error) => panic!("{error}"),
    }
}
fn snapshot(environment: &ManagedEnvironment, base: &Path) -> (IvyJson, IvyJson) {
    let Some(EnvironmentStatus {
        instructions,
        mcp,
        skills,
    }) = environment.status().unwrap()
    else {
        return (IvyJson::object([] as [(&str, IvyJson); 0]), IvyJson::Null);
    };
    for status in [&instructions, &mcp, &skills] {
        let at = field(status, "observedAt").as_str().unwrap();
        assert_eq!(at.len(), 24);
        assert!(at.ends_with('Z'));
    }
    let instruction_home =
        node_path::filesystem_path(field(&instructions, "home").as_js_string().unwrap());
    let mcp_path = node_path::filesystem_path(field(&mcp, "target").as_js_string().unwrap());
    let skill_root = node_path::filesystem_path(field(&skills, "target").as_js_string().unwrap());
    let files = IvyJson::object([
        (
            "instructions",
            file(&instruction_home.join("AGENTS.override.md")),
        ),
        ("mcp", file(&mcp_path)),
        ("skill", file(&skill_root.join("fixture/SKILL.md"))),
    ]);
    let status = IvyJson::object([
        ("instructions", normalize(&instructions, base)),
        (
            "environment",
            IvyJson::object([
                ("mcp", normalize(&mcp, base)),
                ("skills", normalize(&skills, base)),
            ]),
        ),
    ]);
    (status, files)
}
async fn refresh(environment: &ManagedEnvironment, client: Arc<dyn RpcClient>) {
    if let Some(sync) = environment.synchronizers().unwrap() {
        let i = (sync.instructions)(client.clone());
        let m = (sync.mcp)(client.clone());
        let s = (sync.skills)(client);
        tokio::join!(i, m, s);
    }
}
async fn close(environment: &ManagedEnvironment) {
    let phases = environment.shutdown_steps();
    let i = (phases.instructions)();
    let m = (phases.mcp)();
    let s = (phases.skills)();
    let result = tokio::join!(i, m, s);
    result.0.unwrap();
    result.1.unwrap();
    result.2.unwrap();
}
fn calls(client: &Client) -> IvyJson {
    let mut calls = client.calls.lock().unwrap().clone();
    calls.sort_by_key(|v| canonical(v, usize::MAX).unwrap());
    IvyJson::Array(calls)
}
#[tokio::test]
async fn original_prelaunch_fallback_modes_status_and_real_files_match() {
    let root = Root::new();
    let rows = field(original(), "rows").as_array().unwrap();
    assert_eq!(rows.len(), 13);
    for row in rows {
        let base = root.0.join(field(row, "mode").as_str().unwrap());
        let (environment, client) = create(&base, row);
        let home = node_path::from_native(&base.join("home"));
        let error = if flag(row, "before") {
            ivy_agent_manager::native_launch::managed_environment_before_launch(
                environment.clone(),
                client.clone(),
            )(home.clone())
            .await
            .err()
            .map(|e| IvyJson::from(e.code))
            .unwrap_or(IvyJson::Null)
        } else {
            IvyJson::Null
        };
        let owner_home = if flag(row, "noHome") {
            None
        } else if flag(row, "emptyHome") {
            Some(JsString::from(""))
        } else if flag(row, "otherHome") {
            Some(node_path::from_native(&base.join("other-home")))
        } else {
            Some(home)
        };
        environment
            .attach_after_launch(owner_home.as_ref())
            .unwrap();
        let recorded = environment.startup_at().unwrap() > 0.0;
        if flag(row, "refresh") {
            refresh(&environment, client.clone()).await;
        }
        close(&environment).await;
        let (status, files) = snapshot(&environment, &base);
        let actual = IvyJson::object([
            ("error", error),
            ("startupRecorded", recorded.into()),
            (
                "firstLive",
                client
                    .first
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(IvyJson::Null),
            ),
            ("calls", calls(&client)),
            ("status", status),
            ("files", files),
        ]);
        same(&actual, field(row, "expected"));
    }
}
#[tokio::test]
async fn admitted_prelaunch_work_survives_abandoned_waiter_and_is_drained_by_real_close_steps() {
    let root = Root::new();
    let base = root.0.join("abandoned");
    let (environment, client) = create(&base, &IvyJson::object([] as [(&str, IvyJson); 0]));
    let entered = Arc::new(Notify::new());
    let (release, receiver) = watch::channel(false);
    let client = Arc::new(Client {
        calls: Mutex::new(vec![]),
        first: Mutex::new(None),
        paths: client.paths.clone(),
        unavailable: false,
        gate: Some(receiver),
        entered: Some(entered.clone()),
    });
    let run = environment
        .before_launch(node_path::from_native(&base.join("home")), client)
        .unwrap();
    entered.notified().await;
    drop(run);
    let closed = Arc::new(AtomicBool::new(false));
    let closed_flag = closed.clone();
    let owner = environment.clone();
    let close_task = tokio::spawn(async move {
        close(&owner).await;
        closed_flag.store(true, Ordering::Release);
    });
    tokio::task::yield_now().await;
    let closed_while_blocked = closed.load(Ordering::Acquire);
    release.send_replace(true);
    close_task.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while environment.startup_at().unwrap() == 0.0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (status, files) = snapshot(&environment, &base);
    same(
        &IvyJson::object([
            ("closedWhileBlocked", closed_while_blocked.into()),
            ("startupRecorded", true.into()),
            ("status", status),
            ("files", files),
        ]),
        field(original(), "abandoned"),
    );
}
