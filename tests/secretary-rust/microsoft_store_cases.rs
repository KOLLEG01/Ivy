use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use ivy_contracts::{hash::hash_json, json::canonical};
use ivy_sdk::{native_evidence::NativeOwnerIdentity, native_owner::NativeOwner};
use ivy_secretary::{
    journal::{Journal, Limits, Pin},
    microsoft_schema::{self, MicrosoftRecord, validate_microsoft},
    microsoft_store::{
        MicrosoftDocument, MicrosoftEpochBoundaries, MicrosoftGuard, MicrosoftGuardFuture,
        MicrosoftStore,
    },
};
use ivy_store::rusqlite::params;
use std::{cell::Cell, collections::HashMap, path::Path, process::Command};

struct Guard {
    calls: Cell<usize>,
    fail: bool,
}
impl MicrosoftGuard for Guard {
    fn check(&self) -> MicrosoftGuardFuture<'_> {
        Box::pin(async move {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                Err(IvyError::new(
                    "fixture_guard_failed",
                    "Fixture guard failure.",
                ))
            } else {
                Ok(())
            }
        })
    }
}
fn record(key: &IvyJson) -> MicrosoftRecord {
    match key.as_str().unwrap().as_str() {
        "secretary/outlook-head" => MicrosoftRecord::OutlookHead,
        "secretary/outlook-plan" => MicrosoftRecord::OutlookPlan,
        "secretary/outlook-call" => MicrosoftRecord::OutlookCall,
        "secretary/outlook-evidence" => MicrosoftRecord::OutlookEvidence,
        "secretary/outlook-publication" => MicrosoftRecord::OutlookPublication,
        "secretary/teams-head" => MicrosoftRecord::TeamsHead,
        "secretary/teams-plan" => MicrosoftRecord::TeamsPlan,
        "secretary/teams-call" => MicrosoftRecord::TeamsCall,
        "secretary/teams-evidence" => MicrosoftRecord::TeamsEvidence,
        "secretary/teams-publication" => MicrosoftRecord::TeamsPublication,
        _ => panic!("Unknown record"),
    }
}
fn result(value: Result<IvyJson, IvyError>) -> IvyJson {
    match value {
        Ok(value) => IvyJson::object([("value", value)]),
        Err(e) => IvyJson::object([
            ("code", e.code.into()),
            ("message", e.message.into()),
            ("outcome", e.outcome.as_str().into()),
        ]),
    }
}
fn document(value: &IvyJson) -> MicrosoftDocument {
    let pin = value.required("pin").unwrap();
    MicrosoftDocument {
        pin: Pin {
            object_id: pin.required("objectId").unwrap().as_str().unwrap(),
            revision: pin.required("revision").unwrap().as_f64().unwrap() as i64,
        },
        value: value.required("value").unwrap().clone(),
    }
}
fn resolve(value: &IvyJson, results: &[IvyJson]) -> IvyJson {
    match value {
        IvyJson::String(s) => {
            let text = s.to_utf8().unwrap_or_default();
            if let Some(reference) = text.strip_prefix('$') {
                let mut path = reference.split('.');
                if let Some(index) = path.next().and_then(|v| v.parse::<usize>().ok()) {
                    return path
                        .fold(&results[index], |v, key| v.required(key).unwrap())
                        .clone();
                }
            }
            value.clone()
        }
        IvyJson::Array(values) => {
            IvyJson::Array(values.iter().map(|v| resolve(v, results)).collect())
        }
        IvyJson::Object(values) => {
            IvyJson::object(values.iter().map(|(k, v)| (k.clone(), resolve(v, results))))
        }
        _ => value.clone(),
    }
}
fn normalize(value: &IvyJson, ids: &HashMap<ivy_contracts::JsString, IvyJson>) -> IvyJson {
    match value {
        IvyJson::String(s) => ids.get(s).cloned().unwrap_or_else(|| value.clone()),
        IvyJson::Array(values) => {
            IvyJson::Array(values.iter().map(|v| normalize(v, ids)).collect())
        }
        IvyJson::Object(values) => {
            IvyJson::object(values.iter().map(|(k, v)| (k.clone(), normalize(v, ids))))
        }
        _ => value.clone(),
    }
}
fn retained(journal: &Journal) -> (IvyJson, HashMap<ivy_contracts::JsString, IvyJson>) {
    let mut ids = HashMap::new();
    let mut query = journal.db.prepare("SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name").unwrap();
    let documents = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, revision, value) = r.unwrap();
            ids.insert(id.clone().into(), format!("document:{key}:{name}").into());
            IvyJson::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
            ])
        })
        .collect::<Vec<_>>();
    let mut query = journal
        .db
        .prepare("SELECT id,key,name,bytes FROM technical_blobs ORDER BY key,name")
        .unwrap();
    let blobs = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, key, name, bytes) = r.unwrap();
            ids.insert(id.clone().into(), format!("blob:{key}:{name}").into());
            IvyJson::object([
                ("id", id.into()),
                ("key", key.into()),
                ("name", name.into()),
                ("bytes", STANDARD.encode(bytes).into()),
            ])
        })
        .collect::<Vec<_>>();
    let mut query = journal
        .db
        .prepare("SELECT id,revision,value_json FROM technical_revisions")
        .unwrap();
    let mut revisions = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (id, revision, value) = r.unwrap();
            IvyJson::object([
                ("id", id.into()),
                ("revision", IvyJson::Number(revision as f64)),
                ("value", parse(&value, 32 * 1024 * 1024).unwrap()),
            ])
        })
        .collect::<Vec<_>>();
    revisions.sort_by(|a, b| {
        normalize(a.required("id").unwrap(), &ids)
            .as_str()
            .unwrap()
            .cmp(&normalize(b.required("id").unwrap(), &ids).as_str().unwrap())
            .then_with(|| {
                a.required("revision")
                    .unwrap()
                    .as_f64()
                    .unwrap()
                    .partial_cmp(&b.required("revision").unwrap().as_f64().unwrap())
                    .unwrap()
            })
    });
    (
        IvyJson::object([
            ("documents", IvyJson::Array(documents)),
            ("revisions", IvyJson::Array(revisions)),
            ("blobs", IvyJson::Array(blobs)),
        ]),
        ids,
    )
}
fn seed(journal: &Journal, input: &IvyJson) {
    for row in input.required("documents").unwrap().as_array().unwrap() {
        let revision = row.get("revision").and_then(IvyJson::as_f64).unwrap_or(1.) as i64;
        let revisions = row.get("revisions").cloned().unwrap_or_else(|| {
            IvyJson::Array(vec![IvyJson::object([
                ("revision", IvyJson::Number(revision as f64)),
                ("value", row.required("value").unwrap().clone()),
            ])])
        });
        let current = revisions
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r.required("revision").unwrap().as_f64() == Some(revision as f64))
            .unwrap();
        let text = |key| row.required(key).unwrap().as_str().unwrap();
        journal
            .db
            .execute(
                "INSERT INTO technical_documents VALUES(?,?,?,?,?)",
                params![
                    text("id"),
                    text("key"),
                    text("name"),
                    revision,
                    canonical(current.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
                ],
            )
            .unwrap();
        for r in revisions.as_array().unwrap() {
            journal
                .db
                .execute(
                    "INSERT INTO technical_revisions VALUES(?,?,?)",
                    params![
                        text("id"),
                        r.required("revision").unwrap().as_f64().unwrap() as i64,
                        canonical(r.required("value").unwrap(), 32 * 1024 * 1024).unwrap()
                    ],
                )
                .unwrap();
        }
    }
    for row in input.required("blobs").unwrap().as_array().unwrap() {
        let text = |key| row.required(key).unwrap().as_str().unwrap();
        journal
            .db
            .execute(
                "INSERT INTO technical_blobs VALUES(?,?,?,?)",
                params![
                    text("id"),
                    text("key"),
                    text("name"),
                    STANDARD.decode(text("bytes")).unwrap()
                ],
            )
            .unwrap();
    }
}
#[tokio::test]
async fn original_microsoft_schemas_local_graph_and_native_recovery() {
    let mut command = Command::new("node");
    command
        .arg("tests/secretary-rust/microsoft-store-oracle.mjs")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().expect("Original Node oracle required");
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
    let compare = |label: &str, actual: &IvyJson, expected: &IvyJson| {
        assert_eq!(
            canonical(actual, 32 * 1024 * 1024).unwrap(),
            canonical(expected, 32 * 1024 * 1024).unwrap(),
            "{label}"
        )
    };
    for row in corpus.required("validation").unwrap().as_array().unwrap() {
        let actual = result(
            validate_microsoft(
                &row.required("name").unwrap().as_str().unwrap(),
                row.required("value").unwrap(),
            )
            .map(|_| IvyJson::Null),
        );
        compare(
            &row.required("label").unwrap().as_str().unwrap(),
            &actual,
            row.required("expected").unwrap(),
        );
    }
    for row in corpus.required("bundles").unwrap().as_array().unwrap() {
        let name = row.required("name").unwrap().as_str().unwrap();
        compare(
            &name,
            &hash_json(&microsoft_schema::bundled(&name).unwrap())
                .unwrap()
                .into(),
            row.required("hash").unwrap(),
        );
    }
    compare(
        "registry",
        &microsoft_schema::microsoft_registry(),
        corpus.required("registry").unwrap(),
    );
    compare(
        "methods",
        &IvyJson::Array(
            microsoft_schema::MICROSOFT_METHODS
                .map(IvyJson::from)
                .to_vec(),
        ),
        corpus.required("methods").unwrap(),
    );
    if let IvyJson::Object(defs) = corpus.required("definitions").unwrap() {
        for (key, value) in defs {
            assert_eq!(
                record(&IvyJson::String(key.clone())).schema(),
                value.as_str().unwrap()
            );
        }
    }
    let rows = corpus.required("rows").unwrap().as_array().unwrap();
    for row in rows {
        let input = row.required("input").unwrap();
        let label = input.required("label").unwrap().as_str().unwrap();
        let journal = Journal::open(Path::new(":memory:"), Limits::default()).unwrap();
        seed(&journal, input);
        let guard = Guard {
            calls: Cell::new(0),
            fail: input.get("guardFailure") == Some(&true.into()),
        };
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
            trace: Mutex::new(vec![]),
        });
        let target = corpus.required("target").unwrap();
        let text = |key| {
            target
                .required(key)
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone()
        };
        let owner = NativeOwner::new(
            Arc::new(ToolClients::new(client.clone())),
            corpus
                .required("caller")
                .unwrap()
                .as_js_string()
                .unwrap()
                .clone(),
            NativeOwnerIdentity {
                service_node_id: text("serviceNodeId"),
                host_id: Some(text("hostId")),
                native_version: text("nativeVersion"),
                native_executable_hash: text("nativeExecutableHash"),
                catalog_hash: text("catalogHash"),
            },
            RequestOptions::default(),
        );
        let store = MicrosoftStore {
            journal: &journal,
            guard: &guard,
        };
        let namespace = input
            .get("namespace")
            .map(|v| v.as_str().unwrap())
            .unwrap_or_else(|| "secretary/outlook".into());
        let boundaries = MicrosoftEpochBoundaries {
            journal: &journal,
            owner: &owner,
            namespace: &namespace,
        };
        let plan = IvyJson::object([
            ("objectId", "plan".into()),
            ("revision", IvyJson::Number(1.0)),
        ]);
        let mut results = vec![];
        for raw in input.required("actions").unwrap().as_array().unwrap() {
            let action = resolve(raw, &results);
            let value = async {
                let key = || record(action.required("key").unwrap());
                let immutable = action.get("immutable") == Some(&true.into());
                let plan = action.get("plan").unwrap_or(&plan);
                let slot = action.get("slot").cloned().unwrap_or(IvyJson::Number(0.0));
                Ok(match action.required("method")?.as_str()?.as_str() {
                    "read" => store
                        .read(
                            key(),
                            action.required("selected")?,
                            immutable,
                            action.get("name").and_then(IvyJson::as_js_string),
                        )?
                        .value(),
                    "find" => store
                        .find(
                            key(),
                            action.required("name")?.as_js_string().unwrap(),
                            immutable,
                        )?
                        .map_or(IvyJson::Null, |d| d.value()),
                    "create" => store
                        .create(
                            key(),
                            action.required("name")?.as_js_string().unwrap(),
                            action.required("value")?,
                            immutable,
                        )
                        .await?
                        .value(),
                    "amend" => store
                        .amend(
                            key(),
                            &document(action.required("current")?),
                            action.required("value")?,
                        )
                        .await?
                        .value(),
                    "save" => {
                        store
                            .save_operation(plan, &slot, action.required("operation")?)
                            .await?
                    }
                    "operation" => store.operation(plan, &slot, action.required("value")?)?,
                    "history" => {
                        store.check_call_history(
                            &MicrosoftDocument {
                                pin: Pin {
                                    object_id: "call".into(),
                                    revision: 1,
                                },
                                value: action.required("value")?.clone(),
                            },
                            &owner,
                        )?;
                        IvyJson::Null
                    }
                    method @ ("boundaryRead" | "boundaryRecord") => {
                        // Defaults are the exact original call/absence frozen in the oracle.
                        let original = IvyJson::object([
                            ("operationId", "original-operation".into()),
                            ("method", "account/read".into()),
                            ("params", IvyJson::object([("refreshToken", false.into())])),
                            (
                                "definitionHash",
                                format!("sha256:{}", "d".repeat(64)).into(),
                            ),
                        ]);
                        let call = action.get("call").unwrap_or(&original);
                        let previous = action
                            .get("previousEpoch")
                            .cloned()
                            .unwrap_or("old-epoch".into());
                        if method == "boundaryRead" {
                            boundaries
                                .read(plan, call, &previous)?
                                .unwrap_or(IvyJson::Null)
                        } else {
                            let absent = IvyJson::object([
                                ("kind", "agent_operation_absent".into()),
                                ("operationId", "original-operation".into()),
                                ("serviceNodeId", "fixture.agent".into()),
                                ("epoch", "next-epoch".into()),
                            ]);
                            boundaries
                                .record(
                                    plan,
                                    call,
                                    &previous,
                                    action.get("absence").unwrap_or(&absent),
                                    &guard,
                                )
                                .await?;
                            IvyJson::Null
                        }
                    }
                    other => panic!("Unknown action {other}"),
                })
            }
            .await;
            results.push(result(value));
        }
        assert!(
            client.steps.lock().unwrap().is_empty(),
            "Unconsumed RPC in {label}"
        );
        let (retained, ids) = retained(&journal);
        let actual = normalize(
            &IvyJson::object([
                ("results", IvyJson::Array(results)),
                (
                    "trace",
                    IvyJson::Array(client.trace.lock().unwrap().clone()),
                ),
                ("guardCalls", IvyJson::Number(guard.calls.get() as f64)),
                ("retained", retained),
            ]),
            &ids,
        );
        compare(&label, &actual, row.required("expected").unwrap());
    }
    println!(
        "microsoft-store original comparisons: {} schema cases, {} bundles, {} workflow cases / {} invocations",
        corpus
            .required("validation")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        corpus
            .required("bundles")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        rows.len(),
        rows.iter()
            .map(|r| r
                .required("input")
                .unwrap()
                .required("actions")
                .unwrap()
                .as_array()
                .unwrap()
                .len())
            .sum::<usize>()
    );
}
