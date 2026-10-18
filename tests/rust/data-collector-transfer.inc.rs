//! Real independent service/worker processes, reference Hive and provider routing.
use super::*;
use ivy_contracts::{IvyResult, builtin_schema::json_equal};
use ivy_data_collector::definition_journal::{content, revision};
use ivy_sdk::{
    operation_ids::new_operation_id,
    tools::{InterfaceRequirement, ToolClients},
};
use ivy_store::{rusqlite::OptionalExtension, sqlite};
use tokio::io::AsyncWriteExt;

const SOURCE: &str = "source.collector";
const TARGET: &str = "target.collector";
const TASK: &str = "transfer";

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn database(fixture: &Fixture) -> PathBuf {
    fixture.root.join("data/data-collector.sqlite")
}
fn snapshot(fixture: &Fixture) -> IvyJson {
    let db = sqlite::open(
        &database(fixture),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: true,
        },
    )
    .unwrap();
    let row: Option<String> = db
        .query_row("SELECT document FROM tasks WHERE id=?", [TASK], |r| {
            r.get(0)
        })
        .optional()
        .unwrap();
    row.map_or(IvyJson::Null, |s| parse(&s, usize::MAX).unwrap())
}
fn metadata(fixture: &Fixture, key: &str) -> Option<String> {
    let db = sqlite::open(
        &database(fixture),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: true,
        },
    )
    .unwrap();
    db.query_row("SELECT value FROM metadata WHERE key=?", [key], |r| {
        r.get(0)
    })
    .optional()
    .unwrap()
}
fn markers(fixture: &Fixture) -> IvyJson {
    IvyJson::Array(
        std::fs::read_to_string(fixture.root.join("work/tasks/transfer/marker"))
            .unwrap_or_default()
            .lines()
            .map(|s| parse(s, usize::MAX).unwrap())
            .collect(),
    )
}
async fn wait_row(
    fixture: &Fixture,
    description: &str,
    check: impl Fn(&IvyJson) -> bool,
) -> IvyJson {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let row = snapshot(fixture);
        if check(&row) {
            return row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{description}: {row:?}"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
async fn invoke(client: &Arc<dyn RpcClient>, node: &str, mut input: IvyJson) -> IvyResult<IvyJson> {
    let operation = if input.get("action").is_some() {
        let id = if let Some(id) = input.get("operationId").and_then(IvyJson::as_js_string) {
            id.clone()
        } else {
            new_operation_id(client.clone(), None).await?
        };
        input.insert("operationId", IvyJson::String(id.clone()));
        Some(id)
    } else {
        None
    };
    let name = if operation.is_some() {
        "data-collector.update"
    } else {
        "data-collector.read"
    };
    ToolClients::new(client.clone())
        .service_tools(
            node.into(),
            &[InterfaceRequirement {
                namespace: "data-collector".into(),
                interface_version: "1.0.0".into(),
            }],
        )?
        .call(&name.into(), input, operation, Default::default())
        .await
}
async fn assignment(client: &Arc<dyn RpcClient>, job: &IvyJson) -> IvyJson {
    let page = invoke(
        client,
        TARGET,
        IvyJson::object([("view", "definitions".into())]),
    )
    .await
    .unwrap();
    page.required("assignments")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|d| content(d).unwrap().get("jobId") == Some(job))
        .unwrap()
        .clone()
}
async fn published(client: &Arc<dyn RpcClient>, node: &str, operation: &ivy_contracts::JsString) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let value = invoke(
                client,
                node,
                IvyJson::object([
                    ("view", "operation".into()),
                    ("operationId", IvyJson::String(operation.clone())),
                ]),
            )
            .await
            .unwrap();
            let run = value.required("run").unwrap();
            if run.get("status") == Some(&"succeeded".into()) {
                break;
            }
            assert_ne!(run.get("status"), Some(&"failed".into()), "{run:?}");
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap();
}
fn activation(job: &IvyJson, expected: &IvyJson, target: &str, enabled: bool) -> IvyJson {
    IvyJson::object([
        ("action", "activate".into()),
        ("jobId", job.clone()),
        ("definitionRevision", IvyJson::Number(1.0)),
        ("expectedRevision", expected.clone()),
        ("serviceNodeId", target.into()),
        ("enabled", enabled.into()),
    ])
}
async fn stop(fixture: &Fixture, child: &OwnedProcess) {
    fixture.control(&fixture.health().unwrap(), true).await;
    let result = complete(child).await;
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.error_code, None, "{result:?}");
    assert_eq!(result.stderr, "", "{result:?}");
}
async fn launch_service(fixture: &Fixture, original: bool) -> OwnedProcess {
    if !original {
        return launch_bounded(fixture, false, false, 150_000).await;
    }
    let mut environment =
        BTreeMap::from([("IVY_LAUNCH_ID".into(), "collector-original-transfer".into())]);
    if let Some(value) = std::env::var_os("IVY_TEST_PACKAGE_JSON") {
        environment.insert("IVY_TEST_PACKAGE_JSON".into(), value);
    }
    process::spawn(
        &HostCommand {
            executable: "node".into(),
            args: vec![
                "--experimental-transform-types".into(),
                "--disable-warning=ExperimentalWarning".into(),
                repository()
                    .join("tests/fixtures/data-collector-lifecycle-original.mjs")
                    .into(),
                fixture.root.join("config.json").into(),
            ],
            cwd: None,
            timeout_ms: 150_000,
        },
        &fixture.root.join("artifact"),
        &BTreeMap::from([("node".into(), node())]),
        LaunchContext {
            environment,
            job_launcher: std::env::var_os("IVY_TEST_JOB_LAUNCHER").map(PathBuf::from),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn scenario(original: bool) -> IvyJson {
    let mut source = Fixture::new(false).await;
    let mut target = Fixture::new(false).await;
    let mut hive = Command::new(node())
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "--import",
            "./tests/fixtures/data-collector-source-loader.mjs",
            "tests/fixtures/data-collector-hive-peer.mjs",
        ])
        .arg(&source.root)
        .current_dir(repository())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(35),
        BufReader::new(hive.stdout.take().unwrap()).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !line.is_empty(),
        "Reference Hive exited: {:?}",
        hive.try_wait()
    );
    let url = model::text(&parse(&line, usize::MAX).unwrap(), "url").unwrap();
    let client: Arc<dyn RpcClient> = Arc::new(
        HiveClient::new(
            &url,
            ClientOptions {
                credential: Some("synthetic-only".into()),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    for (fixture, id) in [(&mut source, SOURCE), (&mut target, TARGET)] {
        fixture.config.insert("publicBaseUrl", url.clone().into());
        fixture.config.insert("serviceNodeId", id.into());
        fixture
            .config
            .insert("hostId", id.split('.').next().unwrap().into());
        let mut settings = fixture.config.required("settings").unwrap().clone();
        settings.insert("rootName", format!("{id} results").into());
        fixture.config.insert("settings", settings);
        atomic_json(fixture.root.join("config.json"), fixture.config.clone())
            .await
            .unwrap();
        stage(fixture).await;
        if original && cfg!(windows) {
            std::fs::copy(
                std::env::var_os("IVY_TEST_JOB_LAUNCHER").unwrap(),
                fixture.root.join("artifact/dist/native/ivy-job.exe"),
            )
            .unwrap();
        }
    }
    let mut source_process = launch_service(&source, original).await;
    let mut target_process = launch_service(&target, original).await;
    let source_health = ready(&source, &source_process, None).await;
    let target_health = ready(&target, &target_process, None).await;
    assert_ne!(source_health.get("pid"), target_health.get("pid"));
    assert_ne!(source_health.get("bootId"), target_health.get("bootId"));
    let source_root = invoke(&client, SOURCE, IvyJson::object([("view", "tasks".into())]))
        .await
        .unwrap()
        .required("rootObjectId")
        .unwrap()
        .clone();
    let target_root = invoke(&client, TARGET, IvyJson::object([("view", "tasks".into())]))
        .await
        .unwrap()
        .required("rootObjectId")
        .unwrap()
        .clone();
    assert_ne!(source_root, target_root);

    let mut task = parse(r#"{"id":"transfer","name":"Transfer","enabled":false,"intervalSeconds":3600,"timeoutSeconds":10,"memoryMb":64,"dependencies":{},"config":{},"secretNames":[],"script":"import {appendFileSync} from 'node:fs'; export default async ({state,input,signal})=>{if(input?.hold){appendFileSync('marker',JSON.stringify({hold:true})+'\\n');await new Promise(resolve=>signal.addEventListener('abort',resolve,{once:true}));}const cursor=(state?.cursor??0)+1;appendFileSync('marker',JSON.stringify({cursor})+'\\n');return {data:{cursor,previous:state?.cursor??null},state:{cursor}};}"}"#, usize::MAX).unwrap();
    let settings = schema::parse_schema(
        "settingsSchema",
        source.config.required("settings").unwrap(),
    )
    .unwrap();
    task.insert("retention", settings.required("retention").unwrap().clone());
    let saved = invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "save".into()),
            ("expectedRevision", IvyJson::Number(0.0)),
            ("task", task),
        ]),
    )
    .await
    .unwrap();
    let job = saved.required("jobId").unwrap().clone();
    invoke(
        &client,
        SOURCE,
        activation(&job, &IvyJson::Number(0.0), SOURCE, true),
    )
    .await
    .unwrap();
    invoke(
        &client,
        SOURCE,
        IvyJson::object([("action", "run".into()), ("id", TASK.into())]),
    )
    .await
    .unwrap();
    wait_row(&source, "first real worker commit", |r| {
        r.get("state") == Some(&IvyJson::object([("cursor", IvyJson::Number(1.0))]))
    })
    .await;
    // Wait for the actual original/native background reconciliation to bind the
    // result before taking an assignment revision for the transfer transaction.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let a = assignment(&client, &job).await;
            if content(&a)
                .unwrap()
                .get("resultObjectId")
                .is_some_and(|r| !r.is_null())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    })
    .await
    .unwrap();
    let before = assignment(&client, &job).await;
    let rejected = invoke(
        &client,
        TARGET,
        activation(&job, revision(&before).unwrap(), TARGET, false),
    )
    .await
    .unwrap_err();
    assert_eq!(rejected.code, "outcome_unknown");
    assert!(snapshot(&target).is_null());
    // An actual worker must be cancelled and its publication drained before the
    // disabled revision can become a quiescent handoff source.
    invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "run".into()),
            ("id", TASK.into()),
            ("input", IvyJson::object([("hold", true.into())])),
        ]),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while markers(&source).as_array().unwrap().len() != 2 {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    let disabled = invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "disable".into()),
            ("id", TASK.into()),
            ("expectedRevision", IvyJson::Number(1.0)),
        ]),
    )
    .await
    .unwrap();
    let disabled_revision = disabled
        .required("assignment")
        .unwrap()
        .required("revision")
        .unwrap()
        .clone();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = invoke(
                &client,
                TARGET,
                IvyJson::object([("view", "definitions".into())]),
            )
            .await
            .unwrap();
            if page
                .required("observations")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .any(|d| {
                    let c = content(d).unwrap();
                    c.get("jobId") == Some(&job)
                        && c.get("serviceNodeId") == Some(&SOURCE.into())
                        && c.get("assignmentRevision") == Some(&disabled_revision)
                        && c.get("quiesced") == Some(&true.into())
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    })
    .await
    .unwrap();
    invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("action", "freezeHandoff".into()),
            ("jobId", job.clone()),
            ("expectedRevision", disabled_revision.clone()),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        invoke(
            &client,
            SOURCE,
            IvyJson::object([("action", "run".into()), ("id", TASK.into())])
        )
        .await
        .unwrap_err()
        .code,
        "task_busy"
    );
    let auth = source.root.join("work/tasks/transfer/.auth");
    std::fs::create_dir_all(&auth).unwrap();
    std::fs::write(auth.join("token"), "synthetic-cache").unwrap();
    let handoff = invoke(
        &client,
        SOURCE,
        IvyJson::object([
            ("view", "handoff".into()),
            ("jobId", job.clone()),
            ("expectedRevision", disabled_revision.clone()),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        handoff.get("state"),
        Some(&IvyJson::object([("cursor", IvyJson::Number(1.0))]))
    );
    assert!(
        handoff
            .get("authenticationFingerprint")
            .is_some_and(|x| !x.is_null())
    );
    assert_eq!(
        invoke(
            &client,
            TARGET,
            activation(&job, &disabled_revision, TARGET, false)
        )
        .await
        .unwrap_err()
        .code,
        "authentication_required"
    );
    assert!(snapshot(&target).is_null());
    assert!(!target.root.join("work/tasks/transfer/.auth").exists());
    stop(&source, &source_process).await;
    let unavailable = invoke(
        &client,
        TARGET,
        activation(&job, &disabled_revision, TARGET, false),
    )
    .await
    .unwrap_err();
    assert_eq!(unavailable.code, "service_unavailable");
    assert_eq!(
        revision(&assignment(&client, &job).await).unwrap(),
        &disabled_revision
    );
    source_process = launch_service(&source, original).await;
    ready(&source, &source_process, source_health.get("bootId")).await;
    let target_auth = target.root.join("work/tasks/transfer/.auth");
    std::fs::create_dir_all(&target_auth).unwrap();
    std::fs::copy(auth.join("token"), target_auth.join("token")).unwrap();
    let mut movement = activation(&job, &disabled_revision, TARGET, false);
    movement.insert(
        "operationId",
        IvyJson::String(new_operation_id(client.clone(), None).await.unwrap()),
    );
    // Source holds its mutation queue while target preparation calls back into
    // its read-only handoff route; this must not deadlock across providers.
    let moved = invoke(&client, SOURCE, movement.clone()).await.unwrap();
    assert_eq!(moved.get("adoptionError"), Some(&IvyJson::Null));
    let moved_revision = moved
        .required("assignment")
        .unwrap()
        .required("revision")
        .unwrap()
        .clone();
    let adopted = wait_row(&target, "background target adoption", |r| {
        r.get("definition")
            .and_then(|d| d.get("assignmentRevision"))
            == Some(&moved_revision)
    })
    .await;
    assert_eq!(adopted.get("state"), handoff.get("state"));
    assert_eq!(adopted.get("nextAt"), handoff.get("nextAt"));
    assert_eq!(adopted.get("resultRootId"), Some(&source_root));
    assert_eq!(
        adopted.get("resultObjectId"),
        snapshot(&source).get("resultObjectId")
    );
    assert_eq!(
        metadata(
            &target,
            &format!("handoff-consumed:{}", job.as_str().unwrap())
        ),
        Some("true".into())
    );
    assert_eq!(
        std::fs::read(target_auth.join("token")).unwrap(),
        b"synthetic-cache"
    );
    // The source's persisted freeze survives restart and release to another node.
    assert_eq!(
        invoke(
            &client,
            SOURCE,
            IvyJson::object([("action", "run".into()), ("id", TASK.into())])
        )
        .await
        .unwrap_err()
        .code,
        "task_busy"
    );
    let run_id = new_operation_id(client.clone(), None).await.unwrap();
    let run = IvyJson::object([
        ("action", "run".into()),
        ("id", TASK.into()),
        ("operationId", IvyJson::String(run_id.clone())),
    ]);
    invoke(&client, TARGET, run.clone()).await.unwrap();
    invoke(&client, TARGET, run).await.unwrap();
    let advanced = wait_row(&target, "transferred cursor executed once", |r| {
        r.get("state") == Some(&IvyJson::object([("cursor", IvyJson::Number(2.0))]))
    })
    .await;
    published(&client, TARGET, &run_id).await;
    let result = invoke(
        &client,
        TARGET,
        IvyJson::object([("view", "result".into()), ("id", TASK.into())]),
    )
    .await
    .unwrap();
    assert_eq!(
        result.required("object").unwrap().get("parentId"),
        Some(&source_root)
    );
    assert_eq!(
        content(&result).unwrap().get("data"),
        Some(&IvyJson::object([
            ("cursor", IvyJson::Number(2.0)),
            ("previous", IvyJson::Number(1.0))
        ]))
    );
    let replay = invoke(&client, SOURCE, movement).await.unwrap();
    assert!(json_equal(&replay, &moved));
    assert_eq!(snapshot(&target).get("state"), advanced.get("state"));
    assert_eq!(snapshot(&target).get("nextAt"), advanced.get("nextAt"));
    stop(&target, &target_process).await;
    let stopped_row = snapshot(&target);
    target_process = launch_service(&target, original).await;
    let restarted_health = ready(&target, &target_process, target_health.get("bootId")).await;
    assert_eq!(snapshot(&target).get("state"), stopped_row.get("state"));
    assert_eq!(snapshot(&target).get("nextAt"), stopped_row.get("nextAt"));
    assert_eq!(
        markers(&source),
        parse(r#"[{"cursor":1},{"hold":true}]"#, usize::MAX).unwrap()
    );
    assert_eq!(
        markers(&target),
        parse(r#"[{"cursor":2}]"#, usize::MAX).unwrap()
    );
    // Cache loss is injected only after confirmed process/tree completion. Keep
    // the real durable observation and consumed-transfer metadata intact.
    stop(&target, &target_process).await;
    let db = sqlite::open(
        &database(&target),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: false,
        },
    )
    .unwrap();
    db.execute("DELETE FROM tasks WHERE id=?", [TASK]).unwrap();
    drop(db);
    target_process = launch_service(&target, original).await;
    ready(&target, &target_process, restarted_health.get("bootId")).await;
    assert!(snapshot(&target).is_null());
    let visible = invoke(
        &client,
        TARGET,
        IvyJson::object([("view", "task".into()), ("id", TASK.into())]),
    )
    .await
    .unwrap();
    assert_eq!(
        visible.get("adoptionError"),
        Some(&"recovery_required".into())
    );
    assert_eq!(
        markers(&target),
        parse(r#"[{"cursor":2}]"#, usize::MAX).unwrap()
    );
    let current = assignment(&client, &job).await;
    let recovery = invoke(
        &client,
        TARGET,
        IvyJson::object([
            ("action", "recover".into()),
            ("jobId", job.clone()),
            ("expectedRevision", revision(&current).unwrap().clone()),
            ("confirmedStopped", true.into()),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(recovery.get("recovered"), Some(&job));
    let recovered = snapshot(&target);
    assert_eq!(recovered.get("state"), Some(&IvyJson::Null));
    assert_eq!(
        metadata(
            &target,
            &format!("handoff-consumed:{}", job.as_str().unwrap())
        ),
        Some("true".into())
    );
    stop(&source, &source_process).await;
    stop(&target, &target_process).await;
    assert_eq!(
        markers(&source),
        parse(r#"[{"cursor":1},{"hold":true}]"#, usize::MAX).unwrap()
    );
    assert_eq!(
        markers(&target),
        parse(r#"[{"cursor":2}]"#, usize::MAX).unwrap()
    );
    hive.stdin
        .take()
        .unwrap()
        .write_all(b"close\n")
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(12), hive.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    IvyJson::object([
        ("enabledSourceRefusal", rejected.code.into()),
        ("unreachableSourceRefusal", unavailable.code.into()),
        ("disabledRevision", disabled_revision),
        ("movedRevision", moved_revision),
        ("sourceMarkers", markers(&source)),
        ("targetMarkers", markers(&target)),
        (
            "transferredState",
            adopted.required("state").unwrap().clone(),
        ),
        ("advancedState", advanced.required("state").unwrap().clone()),
        (
            "recoveredState",
            recovered.required("state").unwrap().clone(),
        ),
    ])
}

#[tokio::test]
async fn isolated_services_match_original_worker_transfer_restart_and_lost_cache_recovery() {
    tokio::time::timeout(Duration::from_secs(210), async {
        let original = scenario(true).await;
        let native = scenario(false).await;
        assert!(
            json_equal(&native, &original),
            "native {native:?}; original {original:?}"
        );
    })
    .await
    .unwrap();
}
