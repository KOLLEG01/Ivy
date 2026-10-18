use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    agent_schema::{NativeFrameKind, validate_agent, validate_agent_frame},
    json::{parse, stringify},
};
use ivy_sdk::{native_evidence::*, native_operation::*, rpc::RequestFuture};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn string(value: &IvyJson, key: &str) -> JsString {
    field(value, key).as_js_string().unwrap().clone()
}
fn owner(value: &IvyJson) -> NativeOwnerIdentity {
    NativeOwnerIdentity {
        service_node_id: string(value, "serviceNodeId"),
        host_id: value.get("hostId").and_then(IvyJson::as_js_string).cloned(),
        native_version: string(value, "nativeVersion"),
        native_executable_hash: string(value, "nativeExecutableHash"),
        catalog_hash: string(value, "catalogHash"),
    }
}
fn identity(value: &IvyJson) -> NativeOperationIdentity {
    NativeOperationIdentity {
        owner: owner(value),
        caller_principal_id: string(value, "callerPrincipalId"),
        operation_id: string(value, "operationId"),
        method: string(value, "method"),
        params: field(value, "params").clone(),
    }
}
fn failure(error: IvyError) -> IvyJson {
    IvyJson::object([
        ("ok", false.into()),
        (
            "error",
            IvyJson::object([
                ("code", IvyJson::String(error.code_js())),
                ("message", IvyJson::String(error.message_js())),
                ("outcome", error.outcome.as_str().into()),
            ]),
        ),
    ])
}
fn success(value: IvyJson) -> IvyJson {
    IvyJson::object([("ok", true.into()), ("value", value)])
}
fn checked(result: IvyResult<()>) -> IvyJson {
    match result {
        Ok(()) => success(IvyJson::Null),
        Err(error) => failure(error),
    }
}
fn call_json(call: &NativeOperationCall) -> IvyJson {
    IvyJson::object([
        ("operationId", IvyJson::String(call.operation_id.clone())),
        ("method", IvyJson::String(call.method.clone())),
        ("params", call.params.clone()),
        (
            "definitionHash",
            IvyJson::String(call.definition_hash.clone()),
        ),
    ])
}
type Trace = Arc<Mutex<Vec<IvyJson>>>;
struct Owner {
    expected: NativeOperationIdentity,
    reads: Mutex<VecDeque<IvyJson>>,
    trace: Trace,
}
impl NativeOperationOwner for Owner {
    fn operation<'a>(&'a self, call: &'a NativeOperationCall) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(IvyJson::object([("read", call_json(call))]));
            let value = self
                .reads
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected read");
            if let Some(code) = value.get("throw") {
                let code = code.as_str()?;
                let outcome = if code == "outcome_unknown" {
                    Outcome::Unknown
                } else {
                    Outcome::NotExecuted
                };
                return Err(IvyError::new(code, "lookup failed").with_outcome(outcome));
            }
            Ok(value)
        })
    }
    fn check_operation(&self, value: &IvyJson, call: &NativeOperationCall) -> IvyResult<()> {
        self.trace.lock().unwrap().push("check".into());
        let mut expected = self.expected.clone();
        expected.operation_id = call.operation_id.clone();
        expected.method = call.method.clone();
        expected.params = call.params.clone();
        validate_native_operation(value, &expected, None)
    }
}
struct Handler {
    scenario: IvyJson,
    trace: Trace,
}
impl NativeAbsentHandler for Handler {
    fn on_absent<'a>(
        &'a self,
        call: NativeOperationCall,
        absence: &'a IvyJson,
    ) -> SubmissionFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("submit", call_json(&call)),
                ("absence", absence.clone()),
            ]));
            if self.scenario.get("submitError") == Some(&IvyJson::Bool(true)) {
                return Err(IvyError::new("claim_changed", "guard failed"));
            }
            Ok(self
                .scenario
                .get("submit")
                .filter(|v| !v.is_null())
                .cloned())
        })
    }
}
async fn reconciliation(input: &IvyJson) -> IvyJson {
    let scenario = field(input, "scenario");
    let trace = Trace::default();
    let owner = Owner {
        expected: identity(field(input, "expected")),
        reads: Mutex::new(
            field(scenario, "reads")
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
        ),
        trace: trace.clone(),
    };
    let call = field(input, "call");
    let call = NativeOperationCall {
        operation_id: string(call, "operationId"),
        method: string(call, "method"),
        params: field(call, "params").clone(),
        definition_hash: string(call, "definitionHash"),
    };
    let handler = Handler {
        scenario: scenario.clone(),
        trace: trace.clone(),
    };
    let on_absent = if scenario.get("submit").is_some() || scenario.get("submitError").is_some() {
        Some(&handler as &dyn NativeAbsentHandler)
    } else {
        None
    };
    let retention = trace.clone();
    let fail = scenario.get("retainError") == Some(&IvyJson::Bool(true));
    let journal = NativeOperationJournal {
        current: 7,
        previous: scenario.get("previous").cloned(),
        was_observed: scenario.get("wasObserved") == Some(&IvyJson::Bool(true)),
        retain: move |value| {
            retention
                .lock()
                .unwrap()
                .push(IvyJson::object([("retain", value)]));
            async move {
                if fail {
                    Err(IvyError::new("outcome_unknown", "save failed")
                        .with_outcome(Outcome::Unknown))
                } else {
                    Ok(8)
                }
            }
        },
    };
    let code = scenario
        .get("conflict")
        .map(IvyJson::as_str)
        .transpose()
        .unwrap();
    let result = match reconcile_native_operation(
        &owner,
        &call,
        journal,
        on_absent,
        code.as_deref(),
    )
    .await
    {
        Ok(NativeReconciliation::Observed { value, observation }) => success(IvyJson::object([
            ("kind", "observed".into()),
            ("value", IvyJson::Number(value as f64)),
            ("observation", observation),
        ])),
        Ok(NativeReconciliation::Absent { value, absence }) => success(IvyJson::object([
            ("kind", "absent".into()),
            ("value", IvyJson::Number(value as f64)),
            ("absence", absence),
        ])),
        Err(error) => failure(error),
    };
    IvyJson::object([
        ("trace", IvyJson::Array(trace.lock().unwrap().clone())),
        ("result", result),
    ])
}

#[tokio::test]
async fn native_evidence_and_reconciliation_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-evidence-oracle.mjs",
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() > 4500);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), 40_000_000).unwrap();
        let expected = parse(row["result"].as_str().unwrap(), 40_000_000).unwrap();
        let kind = field(&input, "kind").as_str().unwrap();
        let result = if kind == "reconcile" {
            reconciliation(&input).await
        } else {
            let value = field(&input, "value");
            let other = field(&input, "other");
            let code = input.get("code").map(IvyJson::as_str).transpose().unwrap();
            let code = code.as_deref();
            match kind.as_str() {
                "instant" => match native_instant(value.as_js_string().unwrap(), code) {
                    Ok(at) => success(IvyJson::Number(at as f64)),
                    Err(error) => failure(error),
                },
                "status" => checked(validate_native_status(value, &owner(other), code)),
                "operation" => checked(validate_native_operation(value, &identity(other), code)),
                "read" => checked(validate_native_read(
                    value,
                    &NativeReadIdentity {
                        owner: owner(other),
                        caller_principal_id: string(other, "callerPrincipalId"),
                        epoch: string(other, "epoch"),
                        method: string(other, "method"),
                        params: field(other, "params").clone(),
                    },
                    code,
                )),
                "progress" => checked(validate_native_progress(value, other, code)),
                "frame" => checked(validate_agent_frame(
                    value,
                    if other.as_str().unwrap() == "request-id" {
                        NativeFrameKind::RequestId
                    } else {
                        NativeFrameKind::Reply
                    },
                )),
                "agent" => checked(validate_agent(&other.as_str().unwrap(), value)),
                _ => panic!("unknown fixture"),
            }
        };
        assert_eq!(
            result,
            expected,
            "input={}\nactual={}\nexpected={}",
            row["input"],
            stringify(&result, 40_000_000).unwrap(),
            row["result"]
        );
    }
}

#[test]
fn evidence_validation_keeps_nested_payloads_off_the_native_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let params = parse(&format!("{}0{}", "[".repeat(120), "]".repeat(120)), 1000).unwrap();
            let expected = NativeOperationIdentity {
                owner: NativeOwnerIdentity {
                    service_node_id: "owner".into(),
                    host_id: None,
                    native_version: "version".into(),
                    native_executable_hash: ivy_contracts::hash::digest("exe").into(),
                    catalog_hash: ivy_contracts::hash::digest("catalog").into(),
                },
                caller_principal_id: "caller".into(),
                operation_id: "original".into(),
                method: "method".into(),
                params: params.clone(),
            };
            let hash = ivy_contracts::hash::hash_json(&IvyJson::object([
                ("method", "method".into()),
                ("params", params.clone()),
            ]))
            .unwrap();
            let value = IvyJson::object([
                ("schemaVersion", IvyJson::Number(1.0)),
                ("operationId", "original".into()),
                ("callerPrincipalId", "caller".into()),
                ("serviceNodeId", "owner".into()),
                ("nativeVersion", "version".into()),
                (
                    "nativeExecutableHash",
                    IvyJson::String(expected.owner.native_executable_hash.clone()),
                ),
                ("method", "method".into()),
                ("params", params),
                ("requestHash", hash.into()),
                ("phase", "accepted".into()),
                ("createdAt", "2026-01-01T00:00:00.000Z".into()),
                ("updatedAt", "2026-01-01T00:00:00.000Z".into()),
                ("epoch", IvyJson::Null),
                ("requestId", IvyJson::Null),
                ("reply", IvyJson::Null),
                ("code", IvyJson::Null),
            ]);
            validate_native_operation(&value, &expected, None).unwrap();
            validate_native_progress(&value, &value, None).unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}
