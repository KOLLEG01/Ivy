use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString,
    checked_native_contract::CheckedNativeContracts,
    json::{canonical, parse},
    native_contract::NativeContract,
};
use ivy_sdk::{
    native_turn_contents::{
        ContractFuture, NativeTurnOwner, read_native_turn_contents, verify_native_turn_contents,
    },
    rpc::RequestFuture,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Owner {
    replies: Mutex<VecDeque<IvyJson>>,
    trace: Mutex<Vec<IvyJson>>,
    contract: Arc<NativeContract>,
    contract_error: bool,
}
impl Owner {
    fn check(
        &self,
        value: &IvyJson,
        method: &str,
        params: &IvyJson,
        epoch: &JsString,
        allow_error: bool,
    ) -> IvyResult<()> {
        self.trace.lock().unwrap().push(IvyJson::object([
            ("kind", "check".into()),
            ("method", method.into()),
            ("params", params.clone()),
            ("epoch", IvyJson::String(epoch.clone())),
        ]));
        if value.get("method") != Some(&IvyJson::from(method))
            || canonical(value.required("params")?, usize::MAX)? != canonical(params, usize::MAX)?
            || value.get("epoch").and_then(IvyJson::as_js_string) != Some(epoch)
        {
            return Err(IvyError::new(
                "native_evidence_mismatch",
                "Test owner rejected changed query or epoch.",
            ));
        }
        if !allow_error && value.required("reply")?.get("result").is_none() {
            return Err(IvyError::new(
                "native_read_failed",
                "The native read returned its original error.",
            ));
        }
        Ok(())
    }
}
impl NativeTurnOwner for Owner {
    fn check_read(
        &self,
        value: &IvyJson,
        method: &str,
        params: &IvyJson,
        epoch: &JsString,
    ) -> IvyResult<()> {
        self.check(value, method, params, epoch, false)
    }
    fn read<'a>(
        &'a self,
        method: &'a str,
        params: IvyJson,
        epoch: &'a JsString,
        allow_error: bool,
    ) -> RequestFuture<'a> {
        Box::pin(async move {
            self.trace.lock().unwrap().push(IvyJson::object([
                ("kind", "read".into()),
                ("method", method.into()),
                ("params", params.clone()),
                ("epoch", IvyJson::String(epoch.clone())),
                ("allowError", allow_error.into()),
            ]));
            let value =
                self.replies.lock().unwrap().pop_front().ok_or_else(|| {
                    IvyError::new("native_test_exhausted", "Unexpected owner read.")
                })?;
            self.check(&value, method, &params, epoch, allow_error)?;
            Ok(value)
        })
    }
    fn selected_contract(&self) -> ContractFuture<'_> {
        Box::pin(async move {
            self.trace
                .lock()
                .unwrap()
                .push(IvyJson::object([("kind", "contract".into())]));
            if self.contract_error {
                return Err(IvyError::new(
                    "native_test_contract",
                    "Selected catalog unavailable.",
                ));
            }
            Ok(self.contract.clone())
        })
    }
}
#[tokio::test]
async fn original_turn_contents_and_owner_call_order_match_typescript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = std::process::Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/native-turn-contents-oracle.mjs",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(rows.len() >= 80);
    let contract = Arc::new(
        NativeContract::new(
            &parse(
                &std::fs::read_to_string(root.join("specs/native/codex-0.158.0/catalog.json"))
                    .unwrap(),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let retained = CheckedNativeContracts::new(&root);
    for row in rows {
        let input = parse(row["input"].as_str().unwrap(), usize::MAX).unwrap();
        let expected = parse(row["expected"].as_str().unwrap(), usize::MAX).unwrap();
        let owner = Owner {
            replies: Mutex::new(
                input
                    .get("responses")
                    .and_then(IvyJson::as_array)
                    .unwrap_or(&[])
                    .iter()
                    .cloned()
                    .collect(),
            ),
            trace: Mutex::new(Vec::new()),
            contract: contract.clone(),
            contract_error: input
                .get("contractError")
                .and_then(IvyJson::as_bool)
                .unwrap_or(false),
        };
        let result = if input.get("kind") == Some(&IvyJson::from("read")) {
            read_native_turn_contents(
                &owner,
                &retained,
                input.required("snapshot").unwrap(),
                &"thread".into(),
                &"turn".into(),
                input
                    .get("maximum")
                    .and_then(IvyJson::as_f64)
                    .map(|n| n as usize),
            )
            .await
        } else {
            verify_native_turn_contents(
                &owner,
                &retained,
                input.required("value").unwrap(),
                &"thread".into(),
                &"turn".into(),
            )
            .await
            .map(|v| {
                IvyJson::object([
                    ("snapshot", v.snapshot),
                    ("turn", v.turn),
                    ("items", IvyJson::Array(v.items)),
                ])
            })
        };
        let outcome = match result {
            Ok(value) => IvyJson::object([("ok", true.into()), ("value", value)]),
            Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
        };
        let result = IvyJson::object([
            ("outcome", outcome),
            ("trace", IvyJson::Array(owner.trace.into_inner().unwrap())),
        ]);
        assert_eq!(result, expected, "{}", row["label"]);
    }
}

fn turn_snapshot(status: IvyJson, view: &str) -> IvyJson {
    IvyJson::object([
        ("observationId", view.into()),
        ("epoch", "epoch".into()),
        ("method", "thread/turns/list".into()),
        (
            "params",
            IvyJson::object([
                ("threadId", "thread".into()),
                ("cursor", IvyJson::Null),
                ("limit", IvyJson::Number(1.0)),
                ("sortDirection", "desc".into()),
                ("itemsView", view.into()),
            ]),
        ),
        (
            "reply",
            IvyJson::object([(
                "result",
                IvyJson::object([
                    (
                        "data",
                        IvyJson::Array(vec![IvyJson::object([
                            ("id", "turn".into()),
                            ("status", status),
                            ("itemsView", view.into()),
                            ("items", IvyJson::Array(vec![])),
                        ])]),
                    ),
                    ("nextCursor", IvyJson::Null),
                ]),
            )]),
        ),
    ])
}
fn unsupported_page() -> IvyJson {
    parse(r#"{"observationId":"page","epoch":"epoch","method":"thread/items/list","params":{"threadId":"thread","turnId":"turn","cursor":null,"limit":16,"sortDirection":"asc"},"reply":{"error":{"code":-32601,"message":"Unsupported"}}}"#, 1024).unwrap()
}

#[tokio::test]
async fn native_turn_status_requires_a_string_in_verification_and_fallback_reads() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let retained = CheckedNativeContracts::new(&root);
    let contract = Arc::new(
        NativeContract::new(
            &parse(
                &std::fs::read_to_string(root.join("specs/native/codex-0.158.0/catalog.json"))
                    .unwrap(),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let owner = |replies: Vec<IvyJson>| Owner {
        replies: Mutex::new(replies.into()),
        trace: Mutex::new(Vec::new()),
        contract: contract.clone(),
        contract_error: false,
    };
    let valid = ["inProgress", "completed", "failed", "interrupted"];
    let mut statuses: Vec<IvyJson> = valid.iter().map(|status| (*status).into()).collect();
    statuses.extend([
        "unknown".into(),
        IvyJson::Array(vec!["completed".into()]),
        IvyJson::Array(vec![IvyJson::Array(vec!["completed".into()])]),
        IvyJson::object([("toString", IvyJson::Null)]),
        IvyJson::Null,
        IvyJson::Number(42.0),
        false.into(),
        IvyJson::object(Vec::<(String, IvyJson)>::new()),
        IvyJson::String(JsString(vec![0xd800])),
    ]);
    for status in statuses {
        let accepted = status
            .as_js_string()
            .is_some_and(|text| valid.iter().any(|s| text == &JsString::from(*s)));
        let full = turn_snapshot(status.clone(), "full");
        let result = verify_native_turn_contents(
            &owner(vec![]),
            &retained,
            &full,
            &"thread".into(),
            &"turn".into(),
        )
        .await;
        if accepted {
            assert_eq!(result.unwrap().turn.get("status"), Some(&status));
        } else {
            assert_eq!(
                result
                    .err()
                    .expect("non-string/unknown status must fail")
                    .code,
                "native_turn_mismatch",
                "{status:?}"
            );
        }
        let snapshot = turn_snapshot(status.clone(), "notLoaded");
        let reader = owner(vec![unsupported_page(), full.clone()]);
        let result = read_native_turn_contents(
            &reader,
            &retained,
            &snapshot,
            &"thread".into(),
            &"turn".into(),
            None,
        )
        .await;
        if accepted {
            assert_eq!(result.unwrap(), full);
        } else {
            assert_eq!(
                result.unwrap_err().code,
                "native_turn_mismatch",
                "{status:?}"
            );
            assert_eq!(
                reader.trace.lock().unwrap().len(),
                1,
                "reject before requesting any item page"
            );
        }
        if !accepted {
            let reader = owner(vec![unsupported_page(), full]);
            let result = read_native_turn_contents(
                &reader,
                &retained,
                &turn_snapshot("completed".into(), "notLoaded"),
                &"thread".into(),
                &"turn".into(),
                None,
            )
            .await;
            assert_eq!(result.unwrap_err().code, "native_turn_mismatch");
            assert!(
                reader.replies.lock().unwrap().is_empty(),
                "validate the returned full-turn status after both original reads"
            );
        }
    }
}
