use ivy_chat_bridge::diagnostics::{diagnostic_notice, grouped_notice_state, notice_key};
use ivy_contracts::{
    IvyJson as Json,
    json::{parse, stringify},
};

#[path = "fixtures/chat-diagnostic-runtime.rs"]
mod runtime;

fn entry(id: &str, active: bool, time: f64) -> Json {
    Json::object([
        ("code", "unavailable".into()),
        ("label", "reader → service".into()),
        ("firstSeen", Json::Number(time)),
        ("active", Json::Bool(active)),
        (
            "resolvedAt",
            if active {
                Json::Null
            } else {
                Json::Number(time + 10.0)
            },
        ),
        (
            "alert",
            Json::object([
                (
                    "request",
                    Json::object([
                        ("operationId", id.into()),
                        ("text", "original wording".into()),
                        ("opaque", "\u{1f600}".into()),
                    ]),
                ),
                ("queued", Json::Bool(false)),
                ("confirmed", Json::Bool(false)),
            ]),
        ),
        ("recovery", Json::Null),
    ])
}
#[test]
fn grouping_preserves_original_receipts_and_reopen_matches_committed_typescript() {
    let mut cases = Vec::new();
    for active in [false, true] {
        let mut old = entry("original-one", active, 10.0);
        let second = entry("original-two", false, 20.0);
        let mut receipt = entry("retained-recovery", false, 5.0);
        receipt.insert("receiptOnly", Json::Bool(true));
        receipt.insert("recovery", receipt.required("alert").unwrap().clone());
        receipt.insert("alert", Json::Null);
        let mut saved = Json::object([
            ("resource-one", old.clone()),
            ("resource-two", second),
            ("receipt-old", receipt),
        ]);
        // The already grouped row is preferred only when it is in the open pool.
        old.insert("firstSeen", Json::Number(30.0));
        saved.insert(notice_key(&old).unwrap(), old);
        cases.push(saved.clone());
        let reopened = grouped_notice_state(&saved).unwrap();
        assert!(
            stringify(&reopened, usize::MAX)
                .unwrap()
                .contains("original-two")
        );
        assert!(
            stringify(&reopened, usize::MAX)
                .unwrap()
                .contains("retained-recovery")
        );
        cases.push(reopened);
    }
    let diagnostics=parse(r#"[
      {"code":"failure","source":"service:reader"},
      {"code":"failure","source":"service:reader","resource":{"objectId":"private-one"}},
      {"code":"failure","source":"service:reader","resource":{"objectId":"private-two"}},
      {"code":"failure","source":"service:other","resource":{"callerPrincipalId":"reader","serviceNodeId":"dependency"}},
      {"code":"failure","source":"service:reader","resource":{"callerPrincipalId":"reader","serviceNodeId":"reader"}},
      {"code":"failure","source":"service:\ud800","resource":null}
    ]"#,usize::MAX).unwrap();
    let actual = Json::Array(
        cases
            .iter()
            .map(|saved| {
                Json::object([
                    ("state", grouped_notice_state(saved).unwrap()),
                    (
                        "notices",
                        Json::Array(
                            diagnostics
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|d| diagnostic_notice(d).unwrap())
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    );
    let expected = parse(
        include_str!("fixtures/chat-diagnostic-groups.json"),
        usize::MAX,
    )
    .unwrap();
    assert_eq!(actual, expected);
}
