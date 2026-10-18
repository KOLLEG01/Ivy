//! Durable HTTP receipts and ordered publication across real process/epoch changes.
use super::*;

fn runs(fixture: &Fixture) -> Vec<IvyJson> {
    let db = sqlite::open(
        &database(fixture),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: true,
        },
    )
    .unwrap();
    db.prepare("SELECT document FROM runs ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| parse(&r.unwrap(), usize::MAX).unwrap())
        .collect()
}

async fn push(fixture: &Fixture, value: u32, key: &str) -> IvyJson {
    tokio::time::timeout(Duration::from_secs(10), async {
        let body = format!(r#"{{"value":{value}}}"#);
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", fixture.port))
            .await
            .unwrap();
        stream
            .write_all(
                format!(
                    "POST /tasks/{TASK}/run HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nIdempotency-Key: {key}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let response = crate::http::read_reply(stream).await;
        assert_eq!(response.get("status"), Some(&IvyJson::Number(202.0)), "{response:?}");
        response.required("body").unwrap().clone()
    })
    .await
    .unwrap()
}

async fn wait_runs(fixture: &Fixture, count: usize, status: &str) -> Vec<IvyJson> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let values = runs(fixture);
            assert!(
                values.len() <= count,
                "unexpected worker admissions: {values:?}"
            );
            if values.len() == count
                && values
                    .iter()
                    .all(|r| r.get("status") == Some(&status.into()))
            {
                return values;
            }
            assert!(
                values
                    .iter()
                    .all(|r| !["failed", "interrupted", "cancelled"]
                        .iter()
                        .any(|s| r.get("status") == Some(&(*s).into()))),
                "{values:?}"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .unwrap()
}

async fn hive_state(
    hive: &mut tokio::process::Child,
    output: &mut BufReader<tokio::process::ChildStdout>,
    state: &str,
) -> IvyJson {
    hive.stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{state}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(35), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let value = parse(&line, usize::MAX).unwrap();
    assert_eq!(value.get("state"), Some(&state.into()));
    value
}

async fn scenario(original: bool) -> IvyJson {
    let mut fixture = Fixture::new(false).await;
    let mut hive = Command::new(node())
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "--import",
            "./tests/fixtures/data-collector-source-loader.mjs",
            "tests/fixtures/data-collector-hive-peer.mjs",
        ])
        .arg(&fixture.root)
        .current_dir(repository())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = BufReader::new(hive.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(35), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
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
    fixture.config.insert("publicBaseUrl", url.clone().into());
    fixture.config.insert("serviceNodeId", SOURCE.into());
    fixture.config.insert("hostId", "source".into());
    atomic_json(fixture.root.join("config.json"), fixture.config.clone())
        .await
        .unwrap();
    stage(&fixture).await;
    if original && cfg!(windows) {
        std::fs::copy(
            std::env::var_os("IVY_TEST_JOB_LAUNCHER").unwrap(),
            fixture.root.join("artifact/dist/native/ivy-job.exe"),
        )
        .unwrap();
    }
    let mut child = launch_service(&fixture, original).await;
    let initial_health = online(&fixture, &child, None, &client, SOURCE).await;
    let initial_epoch = client
        .request("system.status", model::object(), RequestOptions::default())
        .await
        .unwrap()
        .required("runtimeEpoch")
        .unwrap()
        .clone();
    let mut task = parse(r#"{"id":"transfer","name":"Offline inputs","enabled":false,"intervalSeconds":3600,"timeoutSeconds":10,"memoryMb":64,"dependencies":{},"config":{},"secretNames":[],"allowUnauthenticatedInput":true,"script":"import {appendFileSync} from 'node:fs'; export default ({state,input})=>{const cursor=(state?.cursor??0)+1;const data={cursor,previous:state?.cursor??null,input:input?.value??null};appendFileSync('marker',JSON.stringify(data)+'\\n');return {data,state:{cursor},events:[{name:'first',payload:{cursor,index:0}},{name:'second',payload:{cursor,index:1}}]};}"}"#, usize::MAX).unwrap();
    let settings = schema::parse_schema(
        "settingsSchema",
        fixture.config.required("settings").unwrap(),
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
    // New adoption schedules the first due run an hour away. Commit one actual
    // online worker before exercising the independent offline input queue.
    invoke(
        &client,
        SOURCE,
        IvyJson::object([("action", "run".into()), ("id", TASK.into())]),
    )
    .await
    .unwrap();
    wait_runs(&fixture, 1, "succeeded").await;
    let first = snapshot(&fixture);
    let result_id = first.required("resultObjectId").unwrap().clone();
    assert!(!result_id.is_null());
    hive_state(&mut hive, &mut output, "offline").await;
    let disconnected = fixture.wait_health(false).await;
    assert_eq!(disconnected.get("ready"), Some(&true.into()));
    assert_eq!(disconnected.get("generation"), Some(&IvyJson::Null));

    let receipt = push(&fixture, 2, "offline-one").await;
    assert!(json_equal(
        &push(&fixture, 2, "offline-one").await,
        &receipt
    ));
    wait_row(&fixture, "first offline cursor", |r| {
        r.get("state") == Some(&IvyJson::object([("cursor", IvyJson::Number(2.0))]))
    })
    .await;
    push(&fixture, 3, "offline-two").await;
    wait_row(&fixture, "second offline cursor", |r| {
        r.get("state") == Some(&IvyJson::object([("cursor", IvyJson::Number(3.0))]))
    })
    .await;
    let before_restart = snapshot(&fixture);
    let pending = runs(&fixture)
        .into_iter()
        .filter(|r| r.get("status") == Some(&"publishing".into()))
        .collect::<Vec<_>>();
    assert_eq!(pending.len(), 2);
    assert!(
        pending
            .iter()
            .all(|r| r.get("executionCommitted") == Some(&true.into())
                && r.get("eventIndex") == Some(&IvyJson::Number(0.0)))
    );
    stop(&fixture, &child).await;
    child = launch_service(&fixture, original).await;
    let offline_health = ready(&fixture, &child, initial_health.get("bootId")).await;
    assert_eq!(offline_health.get("generation"), Some(&IvyJson::Null));
    assert_eq!(snapshot(&fixture).get("state"), before_restart.get("state"));
    assert_eq!(
        snapshot(&fixture).get("nextAt"),
        before_restart.get("nextAt")
    );
    assert!(json_equal(
        &push(&fixture, 2, "offline-one").await,
        &receipt
    ));
    push(&fixture, 4, "offline-three").await;
    let latest = wait_row(&fixture, "offline cursor after process restart", |r| {
        r.get("state") == Some(&IvyJson::object([("cursor", IvyJson::Number(4.0))]))
    })
    .await;
    assert_eq!(
        runs(&fixture)
            .iter()
            .filter(|r| r.get("status") == Some(&"publishing".into()))
            .count(),
        3
    );
    let expected_markers = parse(r#"[{"cursor":1,"previous":null,"input":null},{"cursor":2,"previous":1,"input":2},{"cursor":3,"previous":2,"input":3},{"cursor":4,"previous":3,"input":4}]"#, usize::MAX).unwrap();
    assert!(json_equal(&markers(&fixture), &expected_markers));
    // Ordinary Hive restarts preserve runtime_epoch. Inject the separate epoch
    // fault only into the stopped synthetic database, retaining objects/events.
    let changed_epoch = uuid::Uuid::new_v4().to_string();
    let db = sqlite::open(
        &fixture.root.join("hive.sqlite"),
        sqlite::OpenOptions {
            busy_timeout: Duration::from_secs(1),
            read_only: false,
        },
    )
    .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE metadata SET value=? WHERE key='runtime_epoch'",
            [&changed_epoch]
        )
        .unwrap(),
        1
    );
    drop(db);
    let reopened = hive_state(&mut hive, &mut output, "online").await;
    assert_eq!(reopened.get("url"), Some(&url.into()));
    online(&fixture, &child, None, &client, SOURCE).await;
    let epoch = client
        .request("system.status", model::object(), RequestOptions::default())
        .await
        .unwrap()
        .required("runtimeEpoch")
        .unwrap()
        .clone();
    assert_ne!(epoch, initial_epoch);
    assert_eq!(epoch, changed_epoch.into());
    let completed = wait_runs(&fixture, 4, "succeeded").await;
    assert!(
        completed
            .iter()
            .all(|r| r.get("eventIndex") == Some(&IvyJson::Number(2.0)))
    );
    assert_eq!(snapshot(&fixture).get("state"), latest.get("state"));
    assert_eq!(snapshot(&fixture).get("nextAt"), latest.get("nextAt"));
    assert_eq!(snapshot(&fixture).get("resultObjectId"), Some(&result_id));
    assert!(json_equal(
        &push(&fixture, 2, "offline-one").await,
        &receipt
    ));
    assert!(json_equal(&markers(&fixture), &expected_markers));

    let history = client
        .request(
            "objects.history",
            IvyJson::object([
                ("objectId", result_id.clone()),
                ("limit", IvyJson::Number(10.0)),
            ]),
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(history.get("historyComplete"), Some(&true.into()));
    let revisions = history.required("items").unwrap().as_array().unwrap();
    assert_eq!(revisions.len(), 4);
    for (i, revision) in revisions.iter().rev().enumerate() {
        let value = client
            .request(
                "objects.read",
                IvyJson::object([
                    ("objectId", result_id.clone()),
                    ("revision", revision.required("revision").unwrap().clone()),
                ]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        let result = value
            .required("content")
            .unwrap()
            .required("value")
            .unwrap();
        assert_eq!(result.get("runId"), completed[i].get("id"));
        assert!(json_equal(
            result.required("data").unwrap(),
            &expected_markers.as_array().unwrap()[i]
        ));
    }
    // The core object-change events share Hive's durable sequence with emitted
    // Collector events, so this checks result/event interleaving, not timestamps.
    let mut after = IvyJson::Number(0.0);
    let mut timeline = Vec::new();
    let mut event_ids = std::collections::BTreeSet::new();
    loop {
        let batch = client
            .request(
                "events.read",
                IvyJson::object([
                    ("afterSequence", after.clone()),
                    (
                        "filter",
                        IvyJson::object([(
                            "topics",
                            IvyJson::Array(vec![
                                "hive.object.changed".into(),
                                "data-collector.event".into(),
                            ]),
                        )]),
                    ),
                    ("limit", IvyJson::Number(100.0)),
                ]),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(batch.get("gap"), Some(&IvyJson::Null));
        for event in batch.required("items").unwrap().as_array().unwrap() {
            let payload = event.required("payload").unwrap();
            if event.get("topic") == Some(&"hive.object.changed".into()) {
                if payload.get("objectId") == Some(&result_id) {
                    timeline.push(format!(
                        "result:{}",
                        model::number(payload, "revision").unwrap()
                    ));
                }
            } else {
                let cursor =
                    model::number(payload.required("payload").unwrap(), "cursor").unwrap() as usize;
                let index = model::number(payload, "eventIndex").unwrap() as usize;
                assert!((1..=4).contains(&cursor) && index < 2);
                assert_eq!(payload.get("runId"), completed[cursor - 1].get("id"));
                assert_eq!(
                    model::number(payload.required("payload").unwrap(), "index").unwrap() as usize,
                    index
                );
                assert!(
                    event_ids.insert((cursor, index)),
                    "duplicate event {payload:?}"
                );
                timeline.push(format!("event:{cursor}:{index}"));
            }
        }
        if batch.get("hasMore") == Some(&false.into()) {
            break;
        }
        let next = batch.required("throughSequence").unwrap().clone();
        assert_ne!(next, after);
        after = next;
    }
    let expected_timeline = (1..=4)
        .flat_map(|i| {
            [
                format!("result:{i}"),
                format!("event:{i}:0"),
                format!("event:{i}:1"),
            ]
        })
        .collect::<Vec<_>>();
    assert_eq!(timeline, expected_timeline);
    assert_eq!(event_ids.len(), 8);
    stop(&fixture, &child).await;
    assert!(json_equal(&markers(&fixture), &expected_markers));
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
        ("markers", expected_markers),
        (
            "timeline",
            IvyJson::Array(timeline.into_iter().map(IvyJson::from).collect()),
        ),
        ("completed", IvyJson::Number(completed.len() as f64)),
    ])
}

#[tokio::test]
async fn isolated_services_match_original_offline_cursor_receipts_and_ordered_epoch_reconnect() {
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
