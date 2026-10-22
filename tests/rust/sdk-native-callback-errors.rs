use super::*;
use ivy_sdk::native_operation::*;
use std::{cell::Cell, rc::Rc};

// Reuse the existing mocked owner and the operation/status/absence envelopes in
// native-owner-oracle.mjs. These controls never run an original implementation.
struct LocalCause(Rc<Cell<usize>>);
enum LocalFailure<'a> {
    Sdk(IvyError),
    Source(&'a LocalCause),
    Ordinary(&'a LocalCause),
    Mapped(Box<LocalFailure<'a>>),
}
impl From<IvyError> for LocalFailure<'_> {
    fn from(error: IvyError) -> Self {
        Self::Sdk(error)
    }
}
fn raw<'a>(cause: &'a LocalCause, ordinary: bool) -> LocalFailure<'a> {
    if ordinary {
        LocalFailure::Ordinary(cause)
    } else {
        LocalFailure::Source(cause)
    }
}
fn same_raw(error: LocalFailure<'_>, cause: &LocalCause, ordinary: bool) {
    let actual = match (error, ordinary) {
        (LocalFailure::Source(actual), false) | (LocalFailure::Ordinary(actual), true) => actual,
        _ => panic!("callback cause was converted"),
    };
    assert!(std::ptr::eq(actual, cause));
}
fn wire_error(code: &str, outcome: &str) -> IvyJson {
    IvyJson::object([
        ("code", code.into()),
        ("message", "Fixture failure.".into()),
        ("outcome", outcome.into()),
    ])
}
fn rejected(code: &str, outcome: &str) -> IvyJson {
    IvyJson::object([("error", wire_error(code, outcome))])
}
fn value(value: &IvyJson) -> IvyJson {
    IvyJson::object([("value", value.clone())])
}
fn absence() -> IvyJson {
    json(
        r#"{"kind":"agent_operation_absent","operationId":"original","serviceNodeId":"native-agent","epoch":"epoch"}"#,
    )
}
fn absent_step() -> IvyJson {
    let mut error = wire_error("not_found", "not_executed");
    error.insert("details", absence());
    IvyJson::object([("error", error)])
}
fn hash(letter: char) -> IvyJson {
    format!("sha256:{}", letter.to_string().repeat(64)).into()
}
struct Fixture {
    owner: NativeOwner,
    client: Arc<Client>,
    call: NativeOperationCall,
    operation: IvyJson,
}
impl Fixture {
    fn new(steps: impl FnOnce(&IvyJson, &IvyJson) -> Vec<IvyJson>, refresh: bool) -> Self {
        Self::with_input(steps, refresh, |_| {})
    }
    fn with_input(
        steps: impl FnOnce(&IvyJson, &IvyJson) -> Vec<IvyJson>,
        refresh: bool,
        edit: impl FnOnce(&mut IvyJson),
    ) -> Self {
        let target = NativeOwnerIdentity {
            service_node_id: "native-agent".into(),
            host_id: Some("fixture".into()),
            native_version: "version".into(),
            native_executable_hash: hash('e').as_js_string().unwrap().clone(),
            catalog_hash: hash('c').as_js_string().unwrap().clone(),
        };
        let call = NativeOperationCall {
            operation_id: "original".into(),
            method: "thread/read".into(),
            params: json(r#"{"threadId":"thread"}"#),
            definition_hash: hash('b').as_js_string().unwrap().clone(),
        };
        let time = "2026-01-02T03:04:05.006Z";
        let operation = IvyJson::object([
            ("schemaVersion", IvyJson::Number(1.0)),
            ("operationId", IvyJson::String(call.operation_id.clone())),
            ("callerPrincipalId", "caller".into()),
            (
                "serviceNodeId",
                IvyJson::String(target.service_node_id.clone()),
            ),
            (
                "nativeVersion",
                IvyJson::String(target.native_version.clone()),
            ),
            (
                "nativeExecutableHash",
                IvyJson::String(target.native_executable_hash.clone()),
            ),
            ("method", IvyJson::String(call.method.clone())),
            ("params", call.params.clone()),
            (
                "requestHash",
                ivy_contracts::hash::hash_json(&IvyJson::object([
                    ("method", IvyJson::String(call.method.clone())),
                    ("params", call.params.clone()),
                ]))
                .unwrap()
                .into(),
            ),
            ("createdAt", time.into()),
            ("updatedAt", time.into()),
            ("phase", "accepted".into()),
            ("epoch", IvyJson::Null),
            ("requestId", IvyJson::Null),
            ("reply", IvyJson::Null),
            ("code", IvyJson::Null),
        ]);
        let status = IvyJson::object([
            (
                "serviceNodeId",
                IvyJson::String(target.service_node_id.clone()),
            ),
            ("hostId", "fixture".into()),
            (
                "nativeVersion",
                IvyJson::String(target.native_version.clone()),
            ),
            (
                "nativeExecutableHash",
                IvyJson::String(target.native_executable_hash.clone()),
            ),
            ("catalogHash", IvyJson::String(target.catalog_hash.clone())),
            ("epoch", "epoch".into()),
            ("pid", IvyJson::Number(42.0)),
            ("state", "ready".into()),
            ("observedAt", time.into()),
            ("code", IvyJson::Null),
            ("initialized", json("{}")),
            ("pendingInputs", IvyJson::Number(0.0)),
            ("observedMethods", json("[]")),
            (
                "operations",
                json(r#"{"retained":0,"maximum":100,"bytes":0,"maximumBytes":1048576}"#),
            ),
        ]);
        let options = RequestOptions {
            cancel_error: IvyError::new("fixture_cancelled", "Fixture cancellation.")
                .with_outcome(Outcome::Unknown),
            ..Default::default()
        };
        let mut input = IvyJson::object([
            (
                "target",
                IvyJson::object([(
                    "nativeVersion",
                    IvyJson::String(target.native_version.clone()),
                )]),
            ),
            ("steps", IvyJson::Array(steps(&operation, &status))),
        ]);
        if refresh {
            input.insert(
                "hashes",
                IvyJson::object([("agent.invoke", IvyJson::Array(vec![hash('a'), hash('d')]))]),
            );
        }
        edit(&mut input);
        let client = Arc::new(Client {
            input,
            trace: Mutex::new(vec![]),
            calls: Mutex::new(0),
            lists: Mutex::new(HashMap::new()),
            options: options.clone(),
        });
        let owner = NativeOwner::new(
            Arc::new(ToolClients::new(client.clone())),
            "caller".into(),
            target,
            options,
        );
        Self {
            owner,
            client,
            call,
            operation,
        }
    }
    fn calls(&self) -> usize {
        *self.client.calls.lock().unwrap()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reconciliation_preserves_borrowed_non_send_callback_failures() {
    let cause = LocalCause(Rc::new(Cell::new(0)));
    for ordinary in [false, true] {
        for absent in [false, true] {
            let fixture = Fixture::new(
                |op, _| vec![if absent { absent_step() } else { value(op) }],
                false,
            );
            let result = reconcile_native_operation_local_with_error(
                &fixture.owner,
                &fixture.call,
                NativeOperationJournal {
                    current: &cause,
                    previous: None,
                    was_observed: false,
                    retain: |_| async {
                        tokio::task::yield_now().await;
                        cause.0.set(cause.0.get() + 1);
                        Err(raw(&cause, ordinary))
                    },
                },
                Some(|call: NativeOperationCall, observed: IvyJson| {
                    let cause = &cause;
                    let expected = &fixture.call;
                    async move {
                        assert_eq!(call.operation_id, expected.operation_id);
                        assert_eq!(call.method, expected.method);
                        assert_eq!(call.params, expected.params);
                        assert_eq!(call.definition_hash, expected.definition_hash);
                        assert_eq!(observed, absence());
                        tokio::task::yield_now().await;
                        cause.0.set(cause.0.get() + 1);
                        Err(raw(cause, ordinary))
                    }
                }),
                None,
            )
            .await;
            match result {
                Err(error) => same_raw(error, &cause, ordinary),
                Ok(_) => panic!("callback must fail"),
            };
            assert_eq!(fixture.calls(), 1);
        }
    }
    assert_eq!(cause.0.get(), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn reconciliation_skips_saved_receipts_and_refuses_replay_or_regression() {
    let cause = LocalCause(Rc::new(Cell::new(0)));
    for case in [
        "terminal",
        "unchanged",
        "prior-absence",
        "compact-absence",
        "regression",
        "wrong-identity",
        "unknown-read",
    ] {
        let fixture = Fixture::new(
            |op, _| {
                if case == "terminal" {
                    vec![]
                } else {
                    vec![match case {
                        "prior-absence" | "compact-absence" => absent_step(),
                        "wrong-identity" => {
                            let mut op = op.clone();
                            op.insert("callerPrincipalId", "other".into());
                            value(&op)
                        }
                        "unknown-read" => rejected("lookup_failed", "unknown"),
                        _ => value(op),
                    }]
                }
            },
            false,
        );
        let mut previous = fixture.operation.clone();
        match case {
            "terminal" => {
                previous.insert("phase", "failed".into());
                previous.insert("code", "native_error".into());
                previous.insert("epoch", "epoch".into());
                previous.insert("requestId", IvyJson::Number(42.0));
                previous.insert("reply", json(r#"{"error":{"code":-1,"message":"failed"}}"#));
            }
            "regression" => {
                previous.insert("phase", "outcome_unknown".into());
                previous.insert("code", "outcome_unknown".into());
                previous.insert("epoch", "epoch".into());
                previous.insert("requestId", IvyJson::Number(42.0));
            }
            _ => {}
        }
        let result = reconcile_native_operation_local_with_error(
            &fixture.owner,
            &fixture.call,
            NativeOperationJournal {
                current: &cause,
                previous: (!matches!(case, "compact-absence" | "wrong-identity" | "unknown-read"))
                    .then_some(previous),
                was_observed: case == "compact-absence",
                retain: |_| async {
                    panic!("must not retain refused or unchanged evidence");
                    #[allow(unreachable_code)]
                    Ok::<_, LocalFailure<'_>>(&cause)
                },
            },
            Some(|_, _| async {
                panic!("observed work must not authorize replay");
                #[allow(unreachable_code)]
                Ok::<_, LocalFailure<'_>>(None)
            }),
            Some("retained_absence"),
        )
        .await;
        match (case, result) {
            ("terminal" | "unchanged", Ok(NativeReconciliation::Observed { value, .. })) => {
                assert!(std::ptr::eq(value, &cause))
            }
            ("prior-absence" | "compact-absence", Err(LocalFailure::Sdk(e))) => {
                assert_eq!(e.code, "retained_absence")
            }
            ("regression", Err(LocalFailure::Sdk(e))) => {
                assert_eq!(e.code, "native_evidence_regressed")
            }
            ("wrong-identity", Err(LocalFailure::Sdk(e))) => {
                assert_eq!(e.code, "native_evidence_mismatch")
            }
            ("unknown-read", Err(LocalFailure::Sdk(e))) => {
                assert_eq!(e.code, "lookup_failed");
                assert_eq!(e.outcome, Outcome::Unknown);
            }
            _ => panic!("unexpected reconciliation result for {case}"),
        }
        assert_eq!(fixture.calls(), usize::from(case != "terminal"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reconciliation_absence_submission_and_existing_wrappers_agree() {
    for submit in [false, true] {
        for local in [false, true] {
            let fixture = Fixture::new(|op, _| vec![absent_step(), value(op)], false);
            let journal = NativeOperationJournal {
                current: 7,
                previous: None,
                was_observed: false,
                retain: |_| async { Ok(8) },
            };
            let handler = |call: NativeOperationCall, observed: IvyJson| {
                assert_eq!(call.params, fixture.call.params);
                assert_eq!(observed, absence());
                std::future::ready(Ok(submit.then(|| fixture.operation.clone())))
            };
            let result = if local {
                reconcile_native_operation_local_with_error::<_, _, _, _, _, IvyError>(
                    &fixture.owner,
                    &fixture.call,
                    journal,
                    Some(handler),
                    None,
                )
                .await
            } else {
                reconcile_native_operation_local(
                    &fixture.owner,
                    &fixture.call,
                    journal,
                    Some(handler),
                    None,
                )
                .await
            };
            assert!(matches!(
                result,
                Ok(NativeReconciliation::Observed { value: 8, .. })
            ));
            assert_eq!(fixture.calls(), if submit { 1 } else { 2 });
        }
    }
    let fixture = Fixture::new(|_, _| vec![absent_step()], false);
    fn send<F: std::future::Future + Send>(f: F) -> F {
        f
    }
    let result = send(reconcile_native_operation(
        &fixture.owner,
        &fixture.call,
        NativeOperationJournal {
            current: 7,
            previous: None,
            was_observed: false,
            retain: |_| async { Ok(8) },
        },
        None,
        None,
    ))
    .await;
    assert!(matches!(
        result,
        Ok(NativeReconciliation::Absent { value: 7, .. })
    ));
    assert_eq!(fixture.calls(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_maps_only_the_retry_guard_and_following_signal_block() {
    let cause = LocalCause(Rc::new(Cell::new(0)));
    for attempt in [1, 2] {
        for kind in ["source", "ordinary", "signal"] {
            let fixture = Fixture::new(
                |op, status| {
                    vec![
                        rejected("tool_definition_changed", "not_executed"),
                        value(status),
                        value(op),
                    ]
                },
                true,
            );
            let guards = Cell::new(0);
            let mapped = Cell::new(0);
            let result = fixture
                .owner
                .dispatch_local_with_error(
                    &fixture.call,
                    || async {
                        let n = guards.get() + 1;
                        guards.set(n);
                        tokio::task::yield_now().await;
                        if n == attempt {
                            if kind == "signal" {
                                fixture.client.options.cancellation.cancel();
                            } else {
                                return Err(raw(&cause, kind == "ordinary"));
                            }
                        }
                        Ok(())
                    },
                    |error| {
                        mapped.set(mapped.get() + 1);
                        LocalFailure::Mapped(Box::new(error))
                    },
                )
                .await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("guard block must fail"),
            };
            let error = if attempt == 2 {
                match error {
                    LocalFailure::Mapped(error) => *error,
                    _ => panic!("retry mapper omitted"),
                }
            } else {
                error
            };
            if kind == "signal" {
                match error {
                    LocalFailure::Sdk(e) => {
                        assert_eq!(e.code, "fixture_cancelled");
                        assert_eq!(e.outcome, Outcome::Unknown);
                    }
                    _ => panic!("signal error was lost"),
                }
            } else {
                same_raw(error, &cause, kind == "ordinary");
            }
            assert_eq!(guards.get(), attempt);
            assert_eq!(mapped.get(), usize::from(attempt == 2));
            assert_eq!(fixture.calls(), if attempt == 1 { 0 } else { 2 });
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn typed_transport_recovery_and_existing_dispatch_wrappers_agree() {
    for case in [
        "success",
        "unknown",
        "completed",
        "changed-unknown",
        "failure",
        "same-envelope",
        "refresh",
        "second-send",
        "refresh-failure",
        "status-failure",
        "native-changed",
        "initial-native",
    ] {
        let mut traces = vec![];
        let mut results = vec![];
        for mode in 0..3 {
            let fixture = Fixture::with_input(
                |op, status| match case {
                    "success" => vec![value(op)],
                    "unknown" | "completed" => vec![rejected("fixture_failure", case)],
                    "changed-unknown" => vec![rejected("tool_definition_changed", "unknown")],
                    "failure" => vec![rejected("fixture_failure", "not_executed")],
                    "same-envelope" | "refresh-failure" => {
                        vec![rejected("tool_definition_changed", "not_executed")]
                    }
                    "status-failure" => vec![
                        rejected("tool_definition_changed", "not_executed"),
                        rejected("status_failed", "unknown"),
                    ],
                    _ => vec![
                        rejected("tool_definition_changed", "not_executed"),
                        value(status),
                        if case == "second-send" {
                            rejected("tool_definition_changed", "not_executed")
                        } else {
                            value(op)
                        },
                    ],
                },
                !matches!(
                    case,
                    "success"
                        | "unknown"
                        | "completed"
                        | "changed-unknown"
                        | "failure"
                        | "same-envelope"
                ),
                |input| {
                    if case == "refresh-failure" {
                        input.insert(
                            "listErrors",
                            IvyJson::object([(
                                "agent.invoke",
                                IvyJson::Array(vec![
                                    IvyJson::Null,
                                    wire_error("fixture_discovery", "unknown"),
                                ]),
                            )]),
                        );
                    }
                    if case == "native-changed" {
                        input.insert(
                            "hashes",
                            IvyJson::object([
                                ("agent.invoke", IvyJson::Array(vec![hash('a'), hash('d')])),
                                (
                                    "codex.thread/read",
                                    IvyJson::Array(vec![hash('b'), hash('f')]),
                                ),
                            ]),
                        );
                    }
                    if case == "initial-native" {
                        input.insert(
                            "hashes",
                            IvyJson::object([(
                                "codex.thread/read",
                                IvyJson::Array(vec![hash('f')]),
                            )]),
                        );
                    }
                },
            );
            let guard = Guard {
                client: fixture.client.clone(),
                calls: Mutex::new(0),
            };
            fn send<F: std::future::Future + Send>(f: F) -> F {
                f
            }
            let result = match mode {
                0 => send(fixture.owner.dispatch(&fixture.call, &guard)).await,
                1 => {
                    fixture
                        .owner
                        .dispatch_local(&fixture.call, || guard.check())
                        .await
                }
                _ => {
                    fixture
                        .owner
                        .dispatch_local_with_error(
                            &fixture.call,
                            || guard.check(),
                            |_: IvyError| panic!("transport error is not a guard failure"),
                        )
                        .await
                }
            };
            let result = match result {
                Ok(value) => IvyJson::object([
                    ("ok", true.into()),
                    ("value", value.unwrap_or(IvyJson::Null)),
                ]),
                Err(error) => IvyJson::object([("ok", false.into()), ("error", error.to_wire())]),
            };
            match case {
                "success" | "refresh" => assert_eq!(result.get("value"), Some(&fixture.operation)),
                "unknown" | "completed" | "changed-unknown" => {
                    assert_eq!(result.get("value"), Some(&IvyJson::Null))
                }
                _ => assert_eq!(
                    field(field(field(&result, "error"), "data"), "outcome"),
                    &IvyJson::from("not_executed"),
                    "{case} mode {mode}"
                ),
            }
            results.push(result);
            traces.push(fixture.client.trace.lock().unwrap().clone());
        }
        assert!(
            results.windows(2).all(|pair| pair[0] == pair[1]),
            "result {case}"
        );
        assert!(
            traces.windows(2).all(|pair| pair[0] == pair[1]),
            "trace {case}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_wrapper_maps_post_guard_signal_only_on_retry() {
    for attempt in [1, 2] {
        let fixture = Fixture::with_input(
            |op, status| {
                vec![
                    rejected("tool_definition_changed", "not_executed"),
                    value(status),
                    value(op),
                ]
            },
            true,
            |input| {
                input.insert("cancelGuard", IvyJson::Number(attempt as f64));
            },
        );
        let guard = Guard {
            client: fixture.client.clone(),
            calls: Mutex::new(0),
        };
        let result = fixture
            .owner
            .dispatch_local(&fixture.call, || guard.check())
            .await;
        match result {
            Err(error) => {
                assert_eq!(error.code, "fixture_cancelled");
                assert_eq!(
                    error.outcome,
                    if attempt == 1 {
                        Outcome::Unknown
                    } else {
                        Outcome::NotExecuted
                    }
                );
            }
            Ok(_) => panic!("post-guard cancellation must fail"),
        }
        assert_eq!(*guard.calls.lock().unwrap(), attempt);
        assert_eq!(fixture.calls(), if attempt == 1 { 0 } else { 2 });
    }
}
