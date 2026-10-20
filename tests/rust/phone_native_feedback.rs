use super::*;
use ivy_phone_bridge::native_sip::NativeSipCall;

fn feedback(sequence: &str, success: &str) -> String {
    format!(r#"{{"callId":"{CALL}","commandSequence":{sequence},"success":{success}}}"#)
}
pub(super) async fn row(
    dispatcher: &NativeDispatcher,
    method: &str,
    params: &str,
    operation: u64,
) -> IvyJson {
    let reply = dispatcher
        .execute(command(method, params, operation, operation))
        .await;
    let output = if let Some(error) = reply.outcome().error_code() {
        IvyJson::object([("error", error.into())])
    } else {
        IvyJson::object([("json", reply.outcome().result_json().unwrap().into())])
    };
    IvyJson::object([
        ("method", method.into()),
        ("params", parse(params, 65536).unwrap()),
        ("output", output),
    ])
}
pub(super) async fn actions(
    call: &NativeSipCall,
    dispatcher: &NativeDispatcher,
    base: u64,
    capacity: bool,
) -> Vec<IvyJson> {
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let mut rows = vec![
        row(dispatcher, "call.waiting.end", &selected, base).await,
        row(dispatcher, "call.waiting.end", &selected, base + 1).await,
        row(
            dispatcher,
            "call.command.feedback",
            &feedback("1", "true"),
            base + 2,
        )
        .await,
    ];
    for sequence in 1..=3 {
        for digit in ['*', '0', '#'] {
            call.features().digit(digit, InputSource::SipInfo).unwrap();
        }
        assert_eq!(call.features().observation().command_sequence, sequence);
        if sequence == 3 {
            break;
        }
        let sequence = sequence.to_string();
        for success in if sequence == "1" {
            ["false", "true"]
        } else {
            ["true", "false"]
        } {
            rows.push(
                row(
                    dispatcher,
                    "call.command.feedback",
                    &feedback(&sequence, success),
                    base + rows.len() as u64,
                )
                .await,
            );
        }
    }
    for index in [2, 4, 6] {
        assert_eq!(
            rows[index]
                .get("output")
                .unwrap()
                .get("error")
                .unwrap()
                .as_str()
                .unwrap(),
            "runtime_not_ready"
        );
    }
    for index in [3, 5] {
        assert!(rows[index].get("output").unwrap().get("json").is_some());
    }
    if capacity {
        for sequence in 3..=34 {
            if sequence > 3 {
                for digit in ['*', '0', '#'] {
                    call.features().digit(digit, InputSource::SipInfo).unwrap();
                }
            }
            let next = row(
                dispatcher,
                "call.command.feedback",
                &feedback(&sequence.to_string(), "true"),
                base + rows.len() as u64,
            )
            .await;
            if sequence <= 32 {
                assert!(next.get("output").unwrap().get("json").is_some());
            } else {
                assert_eq!(
                    next.get("output")
                        .unwrap()
                        .get("error")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    "operation_outcome_unknown"
                );
            }
            rows.push(next);
            if sequence >= 33 {
                let retry = row(
                    dispatcher,
                    "call.command.feedback",
                    &feedback(&sequence.to_string(), "false"),
                    base + rows.len() as u64,
                )
                .await;
                assert_eq!(
                    retry
                        .get("output")
                        .unwrap()
                        .get("error")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    "runtime_not_ready"
                );
                rows.push(retry);
            }
        }
        assert!(call.features().observation().command_overflow);
    }
    rows
}
pub(super) async fn after_hangup(
    dispatcher: &NativeDispatcher,
    base: u64,
    rows: &mut Vec<IvyJson>,
) {
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    for (method, params) in [
        ("call.waiting.end", selected),
        ("call.command.feedback", feedback("3", "true")),
    ] {
        let next = row(dispatcher, method, &params, base + rows.len() as u64).await;
        assert_eq!(
            next.get("output")
                .unwrap()
                .get("error")
                .unwrap()
                .as_str()
                .unwrap(),
            "runtime_not_ready"
        );
        rows.push(next);
    }
}
pub(super) async fn replay(dispatcher: &NativeDispatcher, base: u64, rows: &[IvyJson]) {
    for (index, expected) in rows.iter().enumerate() {
        let method = expected.get("method").unwrap().as_str().unwrap();
        let params = stringify(expected.get("params").unwrap(), 65536).unwrap();
        let actual = row(dispatcher, &method, &params, base + index as u64).await;
        assert_eq!(
            &actual, expected,
            "immutable original feedback receipt {index}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_waiting_and_feedback_dto_order_and_int64_tokens_match_original() {
    let (owner, factory, routes, params) = actual_owner(46000, 46998);
    let remaining = Arc::new(Remaining::default());
    remaining.close_gate.release();
    let handler = Arc::new(NativeSipCommands::new(
        owner,
        remaining.clone(),
        |_| Box::pin(async { Ok(()) }),
        closed_platform_views(),
    ));
    let selected = format!(r#"{{"callId":"{CALL}"}}"#);
    let mut steps = vec![
        ("call.waiting.end".into(), "{}".into()),
        ("call.command.feedback".into(), "{}".into()),
        ("configure".into(), params),
    ];
    for method in ["call.waiting.end", "call.command.feedback"] {
        for raw in [
            "{}",
            r#"{"callId":null}"#,
            r#"{"callId":42}"#,
            r#"{"callId":"missing"}"#,
        ] {
            steps.push((method.into(), raw.into()));
        }
    }
    steps.push(("call.prepare".into(), selected.clone()));
    for sequence in [
        "null",
        "true",
        r#""1""#,
        "[]",
        "1.0",
        "1e0",
        "9223372036854775808",
        "-9223372036854775809",
        "9223372036854775807",
        "-9223372036854775808",
        "9007199254740993",
        "-0",
        "0",
        "1",
    ] {
        steps.push(("call.command.feedback".into(), feedback(sequence, "true")));
    }
    for success in ["null", "1", r#""true""#, "[]", "false"] {
        steps.push(("call.command.feedback".into(), feedback("1", success)));
    }
    steps.extend([
        (
            "call.command.feedback".into(),
            feedback("1", "true").replace("\"success\":true", "\"success\":true,\"extra\":null"),
        ),
        (
            "call.command.feedback".into(),
            feedback("1", "true").replace("\"callId\"", "\"CallId\""),
        ),
        (
            "call.waiting.end".into(),
            format!(r#"{{"callId":"{CALL}","extra":null}}"#),
        ),
        ("call.waiting.end".into(), selected.clone()),
        ("call.hangup".into(), selected.clone()),
        ("call.waiting.end".into(), selected.clone()),
        ("call.command.feedback".into(), feedback("1", "true")),
        ("call.release".into(), selected),
    ]);
    let original = super::dial::original_raw(&steps);
    let dispatcher = NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap();
    let mut rows = Vec::new();
    for (index, (method, params)) in steps.iter().enumerate() {
        let r = row(&dispatcher, method, params, index as u64 + 1).await;
        rows.push(r.get("output").unwrap().clone());
    }
    trace(
        "native-feedback-validation",
        IvyJson::object([
            (
                "rawInputs",
                format!(
                    "[{}]",
                    steps
                        .iter()
                        .map(|(m, p)| format!(r#"{{"method":"{m}","params":{p}}}"#))
                        .collect::<Vec<_>>()
                        .join(",")
                )
                .into(),
            ),
            ("rust", IvyJson::Array(rows.clone())),
            ("original", original.clone().unwrap_or(IvyJson::Null)),
        ]),
    );
    super::dial::compare(&rows, &original, &steps);
    assert_eq!(factory.starts.load(Ordering::SeqCst), 1);
    assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
    assert!(remaining.methods.lock().unwrap().is_empty());
    handler.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_connected_waiting_and_feedback_keep_original_receipts_after_replacement() {
    for mode in ["initial", "delayed"] {
        let (owner, factory, routes, params) = actual_owner(45000, 45998);
        let params = params.replace(
            "\"transport\":\"udp\"",
            &format!("\"transport\":\"udp\",\"outgoingOfferMode\":\"{mode}\""),
        );
        let remaining = Arc::new(Remaining::default());
        remaining.close_gate.release();
        let handler = Arc::new(NativeSipCommands::new(
            owner.clone(),
            remaining.clone(),
            |_| Box::pin(async { Ok(()) }),
            closed_platform_views(),
        ));
        let dispatcher =
            Arc::new(NativeDispatcher::new(CALL.into(), 128, handler.clone()).unwrap());
        let selected = format!(r#"{{"callId":"{CALL}"}}"#);
        let mut steps = vec![("configure".into(), params)];
        for scenario in [
            "feedback_waiting",
            "feedback_no_waiting",
            "feedback_capacity",
        ] {
            steps.push(("call.prepare".into(), selected.clone()));
            steps.push((
                "fixture.outgoing".into(),
                format!(r#"{{"callId":"{CALL}","scenario":"{scenario}"}}"#),
            ));
        }
        let original = super::dial::original_raw(&steps);
        let mut rows = Vec::new();
        for (index, (method, params)) in steps.iter().enumerate() {
            let result = if method == "fixture.outgoing" {
                let scenario = parse(params, 65536)
                    .unwrap()
                    .get("scenario")
                    .unwrap()
                    .as_str()
                    .unwrap();
                let result = super::dial::outgoing(
                    &owner,
                    dispatcher.clone(),
                    &scenario,
                    1000 + 1000 * index as u64,
                    None,
                    None,
                    None,
                    None,
                )
                .await;
                let actions = result.get("feedbackActions").unwrap().as_array().unwrap();
                if scenario == "feedback_waiting" {
                    assert!(actions[0].get("output").unwrap().get("json").is_some());
                } else {
                    assert_eq!(
                        actions[0]
                            .get("output")
                            .unwrap()
                            .get("error")
                            .unwrap()
                            .as_str()
                            .unwrap(),
                        "operation_outcome_unknown"
                    );
                }
                assert_eq!(
                    actions[1]
                        .get("output")
                        .unwrap()
                        .get("error")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                    "operation_outcome_unknown"
                );
                result
            } else {
                let r = dispatcher
                    .execute(command(method, params, index as u64 + 1, index as u64 + 1))
                    .await;
                value(r.outcome())
            };
            rows.push(IvyJson::object([(
                "json",
                stringify(&result, 65536).unwrap().into(),
            )]));
        }
        trace(
            &format!("native-feedback-{mode}"),
            IvyJson::object([
                ("rust", IvyJson::Array(rows.clone())),
                ("original", original.clone().unwrap_or(IvyJson::Null)),
                ("savedFeedbackReceiptsAfterReplacement", true.into()),
                ("physicalAudioOpens", n(0)),
            ]),
        );
        super::dial::compare(&rows, &original, &steps);
        assert_eq!(factory.starts.load(Ordering::SeqCst), 6);
        assert_eq!(routes.opens.load(Ordering::SeqCst), 0);
        assert!(remaining.methods.lock().unwrap().is_empty());
        handler.shutdown().await.unwrap();
    }
}
