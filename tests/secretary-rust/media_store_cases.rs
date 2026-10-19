use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{
    hash::{digest, hash_json},
    json::canonical,
    operation::parse_operation_id_js,
};
use ivy_sdk::clock::Clock;
use ivy_secretary::{
    engine::SecretaryEngine,
    inbox_evidence::InboxEvidence,
    inbox_recovery::RecoveryScan,
    journal::{Journal, Limits},
    media_spool::{SpoolJournal, Validation},
    media_store::{
        AdmissionFuture, MediaAdmissionGuard, MediaStore, media_registry,
        verify_outlook_media_admission,
    },
    store::Store,
};
use ivy_store::rusqlite::{Connection, params};
use std::{cell::RefCell, path::Path, process::Command};

struct Fixed(i64);
impl Clock for Fixed {
    fn utc_millis(&self) -> i64 {
        self.0
    }
    fn monotonic(&self) -> std::time::Instant {
        std::time::Instant::now()
    }
}
struct NativeUnused;
impl Validation for NativeUnused {
    fn operation(&self, _: &IvyJson) -> Result<(), IvyError> {
        panic!("Publication cannot dispatch native work")
    }
    fn progress(&self, _: &IvyJson, _: &IvyJson) -> Result<(), IvyError> {
        panic!("Publication cannot dispatch native work")
    }
}
struct Guard<'a, 'engine, 'store, 'journal> {
    engine: &'a SecretaryEngine<'engine, 'store, 'journal>,
    input: &'a IvyJson,
    trace: RefCell<Vec<IvyJson>>,
}
impl MediaAdmissionGuard for Guard<'_, '_, '_, '_> {
    fn verify<'a>(&'a self, admission: &'a IvyJson, capture: &'a IvyJson) -> AdmissionFuture<'a> {
        Box::pin(async move {
            self.trace.borrow_mut().push(IvyJson::object([
                ("admission", admission.clone()),
                ("captureOperationId", capture.clone()),
            ]));
            if self.input.get("guardFailure") == Some(&true.into()) {
                return Err(IvyError::new(
                    "fixture_guard_failed",
                    "Fixture guard failure.",
                ));
            }
            if self.input.get("verifyGuard") == Some(&true.into()) {
                verify_outlook_media_admission(
                    self.engine,
                    self.input.required("configuration")?,
                    admission,
                    capture,
                )
                .await?;
            }
            Ok(())
        })
    }
}
fn result(value: Result<IvyJson, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => {
            let mut value = IvyJson::object([
                ("code", e.code.clone().into()),
                ("outcome", e.outcome.as_str().into()),
            ]);
            if e.code != "internal" {
                value.insert("message", e.message.into());
            }
            value
        }
    }
}
fn retained(journal: &Journal) -> IvyJson {
    let mut query = journal
        .db
        .prepare("SELECT key,name,current_revision,value_json FROM documents ORDER BY key,name")
        .unwrap();
    IvyJson::Array(
        query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .unwrap()
            .map(|r| {
                let (key, name, revision, value) = r.unwrap();
                IvyJson::object([
                    ("key", key.into()),
                    ("name", name.into()),
                    ("revision", IvyJson::Number(revision as f64)),
                    ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
                ])
            })
            .collect(),
    )
}
fn brief(value: &IvyJson) -> String {
    canonical(value, 32 * 1024 * 1024)
        .unwrap()
        .chars()
        .take(3000)
        .collect()
}
#[tokio::test]
async fn original_media_publication_admission_coverage_and_payload_recovery() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/media-store-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let corpus = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        32 * 1024 * 1024,
    )
    .unwrap();
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 90);
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let expected = row.required("expected").unwrap();
        let client = Arc::new(Scripted {
            steps: Mutex::new(
                input
                    .required("steps")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::default(),
        });
        let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
        for local in input.required("locals").unwrap().as_array().unwrap() {
            let id = local.required("id").unwrap().as_str().unwrap();
            let key = local.required("key").unwrap().as_str().unwrap();
            let raw = canonical(local.required("value").unwrap(), 32 * 1024 * 1024).unwrap();
            let first = canonical(local.required("first").unwrap(), 32 * 1024 * 1024).unwrap();
            let revision = local.required("revision").unwrap().as_f64().unwrap() as i64;
            journal
                .db
                .execute(
                    "INSERT INTO documents VALUES(?,?,?,?,?,?,?)",
                    params![
                        id,
                        key,
                        local.required("name").unwrap().as_str().unwrap(),
                        revision,
                        0,
                        if key == "secretary/operation" {
                            Some(1)
                        } else {
                            None
                        },
                        raw
                    ],
                )
                .unwrap();
            journal
                .db
                .execute("INSERT INTO revisions VALUES(?,?,?)", params![id, 1, first])
                .unwrap();
            if revision == 2 {
                journal
                    .db
                    .execute("INSERT INTO revisions VALUES(?,?,?)", params![id, 2, raw])
                    .unwrap();
            }
        }
        let store = Store {
            client: client.clone(),
            root: corpus
                .required("rootId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            journal: &journal,
        };
        let node = corpus.required("nodeId").unwrap().as_js_string().unwrap();
        let evidence = InboxEvidence {
            store: &store,
            settings: input.required("settings").unwrap(),
            service_node_id: node,
            cancellation: RequestOptions::default(),
        };
        let clock = Fixed(corpus.required("time").unwrap().as_f64().unwrap() as i64);
        let operation_clock = Fixed(corpus.required("wall").unwrap().as_f64().unwrap() as i64);
        let tools = ToolClients::new(client.clone());
        let issues = RefCell::new(RecoveryScan::default());
        let engine = SecretaryEngine {
            evidence: &evidence,
            clock: &clock,
            operation_clock: &operation_clock,
            runtime_epoch: epoch_fixture(&"fixture-epoch".into()),
            new_uuid: || "00000000-0000-4000-8000-000000000fff".into(),
            tools: &tools,
            issues: &issues,
        };
        let guard = Guard {
            engine: &engine,
            input,
            trace: RefCell::new(vec![]),
        };
        let media = MediaStore {
            engine: &engine,
            admission_guard: &guard,
        };
        let mut spool = SpoolJournal::from_owned_connection(
            Connection::open_in_memory().unwrap(),
            &IvyJson::Null,
            NativeUnused,
        )
        .unwrap();
        let payload = input.required("payload").unwrap();
        let intent = payload.required("intent").unwrap();
        let id = intent
            .required("operationId")
            .unwrap()
            .as_js_string()
            .unwrap();
        let bytes = STANDARD
            .decode(payload.required("bytesBase64").unwrap().as_str().unwrap())
            .unwrap();
        spool
            .prepare(intent, payload.required("materialization").unwrap())
            .unwrap();
        assert!(spool.begin_acquire(id).unwrap());
        spool
            .complete_acquire(id, payload.required("evidence").unwrap(), &bytes)
            .unwrap();
        let args = input.required("args").unwrap().as_array().unwrap();
        let method = input.required("method").unwrap().as_str().unwrap();
        if method == "retainedPayload" {
            spool.db.execute("DELETE FROM attempts", []).unwrap();
            assert!(spool.get(id).unwrap().is_none());
        }
        let mut results = vec![];
        for _ in 0..input.required("repeats").unwrap().as_f64().unwrap() as usize {
            let actual = match method.as_str() {
                "registry" => media_registry(),
                "verifyAdmission" => {
                    verify_outlook_media_admission(&engine, &args[0], &args[1], &args[2])
                        .await
                        .map(|_| IvyJson::Null)
                }
                "publish" => media.publish(&spool, id).await,
                "manifest" => {
                    media
                        .manifest(&args[0], (!args[1].is_null()).then_some(&args[1]))
                        .await
                }
                "payload" => media
                    .payload(&args[0], (!args[1].is_null()).then_some(&args[1]))
                    .await
                    .map(|bytes| {
                        IvyJson::object([
                            ("byteLength", IvyJson::Number(bytes.len() as f64)),
                            ("contentHash", digest(&bytes).into()),
                        ])
                    }),
                "coverage" => media
                    .coverage(&args[0])
                    .await
                    .map(|v| v.unwrap_or(IvyJson::Null)),
                "retainedPayload" => {
                    async {
                        assert!(spool.get(id)?.is_none());
                        let saved = media
                            .coverage(intent.required("item")?)
                            .await?
                            .ok_or_else(IvyError::internal)?;
                        let manifest = &saved
                            .required("value")?
                            .required("attachments")?
                            .as_array()
                            .unwrap()[0]
                            .required("manifest")?;
                        let bytes = media
                            .payload(manifest, Some(intent.required("admission")?))
                            .await?;
                        Ok(IvyJson::object([
                            ("coverage", saved),
                            (
                                "payload",
                                IvyJson::object([
                                    ("byteLength", IvyJson::Number(bytes.len() as f64)),
                                    ("contentHash", digest(&bytes).into()),
                                ]),
                            ),
                            ("spoolAbsent", true.into()),
                        ]))
                    }
                    .await
                }
                "saveCoverage" => media.save_coverage(&args[0], &args[1]).await,
                "flow" => {
                    async {
                        let published = media.publish(&spool, id).await?;
                        let mut value = args[0].clone();
                        let mut attachments =
                            value.required("attachments")?.as_array().unwrap().to_vec();
                        attachments[0].insert("manifest", published.required("pin")?.clone());
                        value.insert("attachments", IvyJson::Array(attachments));
                        let saved = media.save_coverage(&value, &args[1]).await?;
                        let bytes = media
                            .payload(
                                published.required("pin")?,
                                Some(intent.required("admission")?),
                            )
                            .await?;
                        Ok(IvyJson::object([
                            ("published", published),
                            ("saved", saved),
                            (
                                "payload",
                                IvyJson::object([
                                    ("byteLength", IvyJson::Number(bytes.len() as f64)),
                                    ("contentHash", digest(&bytes).into()),
                                ]),
                            ),
                        ]))
                    }
                    .await
                }
                _ => panic!("Unknown media method"),
            };
            results.push(result(actual));
        }
        let mut trace = client.trace.lock().unwrap().clone();
        for call in &mut trace {
            if call.get("method") == Some(&"objects.write".into()) {
                let mut params = call.required("params").unwrap().clone();
                parse_operation_id_js(
                    params
                        .required("mutationId")
                        .unwrap()
                        .as_js_string()
                        .unwrap(),
                )
                .unwrap();
                params.insert("mutationId", "<new-operation>".into());
                call.insert("params", params);
            }
        }
        let actual = IvyJson::object([
            ("results", IvyJson::Array(results)),
            ("trace", IvyJson::Array(trace)),
            ("guards", IvyJson::Array(guard.trace.into_inner())),
            ("retained", retained(&journal)),
        ]);
        if canonical(&actual, 32 * 1024 * 1024).unwrap()
            != canonical(expected, 32 * 1024 * 1024).unwrap()
        {
            for key in ["results", "guards", "retained"] {
                if hash_json(actual.required(key).unwrap()).unwrap()
                    != hash_json(expected.required(key).unwrap()).unwrap()
                {
                    eprintln!(
                        "{label} {key} actual={} expected={}",
                        brief(actual.required(key).unwrap()),
                        brief(expected.required(key).unwrap())
                    );
                }
            }
            for (a, b) in actual
                .required("trace")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .zip(expected.required("trace").unwrap().as_array().unwrap())
            {
                if hash_json(a).unwrap() != hash_json(b).unwrap() {
                    eprintln!(
                        "{label} first RPC mismatch actual={} expected={}",
                        brief(a),
                        brief(b)
                    );
                    break;
                }
            }
            panic!("Original media-store comparison failed: {label}");
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Unconsumed original media RPC: {label}"
        );
    }
    println!("media-store original comparisons: {} cases", rows.len());
}
