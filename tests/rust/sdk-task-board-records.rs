use ivy_contracts::{
    IvyError, IvyJson as Json, IvyResult, JsString, builtin_schema::json_equal, json,
};
use ivy_sdk::{
    upgrade_comment_record, upgrade_configuration_record, upgrade_result_content,
    upgrade_result_record, upgrade_run_record, upgrade_task_record,
};
use std::borrow::Cow;

fn upgrade(helper: &str, input: &Json) -> IvyResult<Json> {
    match helper {
        "upgradeRequirement" => Ok(
            ivy_sdk::task_board_records::upgrade_requirement(Some(input))
                .unwrap()
                .into_owned(),
        ),
        "upgradeConfigurationRecord" => upgrade_configuration_record(input).map(Cow::into_owned),
        "upgradeTaskRecord" => upgrade_task_record(input),
        "upgradeResultContent" => upgrade_result_content(input),
        "upgradeResultRecord" => upgrade_result_record(input),
        _ => panic!("Unknown record helper"),
    }
}

#[test]
fn stored_json_migrations_preserve_records_findings_questions_and_artifacts() {
    let fixture = json::parse(
        include_str!("../fixtures/task-board-records.json"),
        usize::MAX,
    )
    .unwrap();
    let cases = fixture.get("cases").unwrap().as_array().unwrap();
    assert_eq!(cases.len(), 70);
    let mut migrated = 0;
    let mut rejected = 0;
    for case in cases {
        let name = case.get("name").unwrap().as_str().unwrap();
        let helper = case.get("helper").unwrap().as_str().unwrap();
        let input = case.get("input").unwrap();
        let before = json::stringify(input, usize::MAX).unwrap();
        let actual = upgrade(&helper, input);
        let malformed = case.get("expected").unwrap().get("error").is_some();
        if malformed {
            let error = actual.expect_err(&name);
            assert_eq!(error.code, "internal_error", "{name}");
            rejected += 1;
        } else {
            let value = actual.unwrap_or_else(|error| panic!("{name}: {error:?}"));
            let expected = case.get("expected").unwrap().get("value").unwrap();
            assert!(
                json_equal(&value, expected),
                "{name}: actual {}",
                json::stringify(&value, usize::MAX).unwrap()
            );
            migrated += 1;
        }
        assert_eq!(
            json::stringify(input, usize::MAX).unwrap(),
            before,
            "{name}"
        );
    }
    assert!(migrated > 45 && rejected > 10);
}

#[test]
fn public_record_apis_use_json_rust_errors_and_no_op_borrows() {
    type Upgrade = fn(&Json) -> Result<Json, IvyError>;
    let helpers: [Upgrade; 5] = [
        upgrade_comment_record,
        upgrade_run_record,
        upgrade_task_record,
        upgrade_result_record,
        upgrade_result_content,
    ];
    for helper in helpers {
        assert_eq!(helper(&Json::Null).unwrap_err().code, "internal_error");
    }
    assert!(ivy_sdk::task_board_records::upgrade_requirement(None).is_none());
    let configuration = json::parse(
        r#"{"defaults":{"executionRequirement":null},"unknown":"\ud800"}"#,
        1000,
    )
    .unwrap();
    assert!(matches!(
        upgrade_configuration_record(&configuration).unwrap(),
        Cow::Borrowed(_)
    ));
}

fn parse(text: &str) -> Json {
    json::parse(text, usize::MAX).unwrap()
}
fn task() -> Json {
    parse(
        r#"{"schemaVersion":1,"fields":{"control":"agent","executionRequirement":null},"workflowState":"todo","comments":[],"updatedAt":"2026-10-20T00:00:00.000Z"}"#,
    )
}
fn question(sequence: usize) -> Json {
    let mut comment = parse(
        r#"{"commentId":"q","sequence":1,"author":{"serviceNodeId":"example","generation":1},"authorKind":"agent","purpose":"question","recipient":{"kind":"user"},"body":"keep\ud800","requests":[],"responses":[],"replyTo":null}"#,
    );
    comment.insert("sequence", Json::Number(sequence as f64));
    comment
}

#[test]
fn current_task_fields_replace_legacy_gates_without_answering_questions() {
    let mut input = parse(
        r#"{
      "schemaVersion":1,"fields":{"control":"agent","executionRequirement":{"kind":"capability","capabilityKey":"c"},"requiredCapabilities":["c","h","c"],"dependencies":["old","step","old"],"nextReviewAt":"2040-01-01T00:00:00.000Z"},
      "workflowState":"in_progress","claim":null,"comments":[],"updatedAt":"2026-10-20T00:00:00.000Z",
      "nextSteps":[{"id":"work","scope":"Keep the authorized work\ud800","requires":[{"kind":"delivery","taskId":"step","key":"api"},{"kind":"time","at":"2031-01-01T00:00:00.000Z"},{"kind":"task","taskId":"step","until":"done"},{"kind":"time","at":"2030-01-01T00:00:00.000Z"}]}],
      "waiting":{"reason":"user","detail":"legacy context","blocker":{"taskId":"wait"}},"blocker":{"taskId":"legacy"},"questionWait":{},"scheduledCheck":{},"consumedSteps":[],"consumedBlockers":[],"continuation":{},
      "runtimeWait":{"reason":"workflow"},"completionIssue":{"code":"task_board_handoff_missing","attempts":8},"unknown":{"raw":"\udfff"}
    }"#,
    );
    let original_question = question(1);
    input.insert("comments", Json::Array(vec![original_question.clone()]));
    let before = input.clone();
    let current = upgrade_task_record(&input).unwrap();
    assert_eq!(
        current.get("wakeAt"),
        Some(&"2030-01-01T00:00:00.000Z".into())
    );
    assert_eq!(
        current.get("dependencies"),
        Some(&parse(r#"["step","wait","old","legacy"]"#))
    );
    assert_eq!(current.get("workflowState"), Some(&"todo".into()));
    assert_eq!(current.get("runtimeWait"), Some(&Json::Null));
    assert!(current.get("completionIssue").is_none());
    for key in [
        "nextSteps",
        "waiting",
        "blocker",
        "questionWait",
        "scheduledCheck",
        "consumedSteps",
        "consumedBlockers",
        "continuation",
    ] {
        assert!(current.get(key).is_none(), "{key}");
    }
    let fields = current.get("fields").unwrap();
    assert_eq!(
        fields.get("executionRequirement"),
        Some(&parse(r#"{"kind":"automatic"}"#))
    );
    assert_eq!(
        fields.get("requiredCapabilities"),
        Some(&parse(r#"["c","h"]"#))
    );
    assert!(fields.get("nextReviewAt").is_none() && fields.get("dependencies").is_none());
    assert_eq!(current.get("unknown"), input.get("unknown"));
    assert_eq!(
        ivy_sdk::open_task_questions(&current, None).unwrap(),
        vec![original_question]
    );
    let comments = current.get("comments").unwrap().as_array().unwrap();
    assert_eq!(comments.len(), 2);
    let context = comments[1].get("body").unwrap().as_js_string().unwrap();
    assert!(context.contains_ascii("context only") && context.contains_ascii("api"));
    assert_eq!(current, upgrade_task_record(&current).unwrap());
    assert_eq!(input, before);
}

#[test]
fn current_schedules_stopped_timers_claims_and_real_issues_are_preserved() {
    for (state, claimed, control, code, expected_state) in [
        ("waiting", false, "agent", "real_failure", "waiting"),
        (
            "waiting",
            false,
            "agent",
            "task_board_handoff_missing",
            "todo",
        ),
        (
            "waiting",
            true,
            "agent",
            "task_board_recovery_required",
            "waiting",
        ),
        ("in_progress", false, "agent", "real_failure", "todo"),
        ("in_progress", true, "agent", "real_failure", "in_progress"),
        ("in_progress", false, "user", "real_failure", "in_progress"),
    ] {
        let mut input = task();
        input.insert(
            "fields",
            Json::object([
                ("control", control.into()),
                ("executionRequirement", Json::Null),
            ]),
        );
        input.insert("workflowState", state.into());
        input.insert(
            "claim",
            if claimed {
                parse(r#"{"owner":{"serviceNodeId":"example"}}"#)
            } else {
                Json::Null
            },
        );
        input.insert(
            "completionIssue",
            Json::object([
                ("code", code.into()),
                ("attempts", Json::Number(5.0)),
                ("detail", "preserve".into()),
            ]),
        );
        input.insert("wakeAt", Json::Null);
        input.insert("dependencies", Json::Array(Vec::new()));
        input.insert(
            "runtimeWait",
            parse(r#"{"reason":"publication","detail":"actual pending outcome"}"#),
        );
        input.insert("nextSteps", parse(r#"[{"scope":"prior work","requires":[{"kind":"time","at":"2030-01-01T00:00:00.000Z"},{"kind":"task","taskId":"old"}]}]"#));
        let current = upgrade_task_record(&input).unwrap();
        assert_eq!(current.get("wakeAt"), Some(&Json::Null));
        assert_eq!(current.get("dependencies"), Some(&Json::Array(Vec::new())));
        assert_eq!(current.get("runtimeWait"), input.get("runtimeWait"));
        assert_eq!(
            current.get("workflowState"),
            Some(&Json::from(expected_state))
        );
        assert_eq!(current.get("claim"), input.get("claim"));
        if code == "real_failure" {
            let issue = current.get("completionIssue").unwrap();
            assert_eq!(issue.get("code"), Some(&"real_failure".into()));
            assert!(issue.get("attempts").is_none());
        } else {
            assert!(current.get("completionIssue").is_none());
        }
    }
}

#[test]
fn comment_and_run_upgrades_expose_current_content_without_legacy_planning() {
    let comment = parse(
        r#"{"body":"keep\ud800","handoff":{"workflowState":"waiting","nextSteps":[{"scope":"work","requires":[{"kind":"time","at":"2030-01-01T00:00:00.000Z"},{"kind":"task","taskId":"peer"}]}],"wakeAt":null,"dependencies":[]}}"#,
    );
    let current = upgrade_comment_record(&comment).unwrap();
    assert_eq!(current.get("body"), comment.get("body"));
    assert!(json_equal(
        current.get("handoff").unwrap(),
        &parse(r#"{"workflowState":"waiting","wakeAt":null,"dependencies":[]}"#)
    ));
    assert_eq!(current, upgrade_comment_record(&current).unwrap());
    let run = parse(
        r#"{"unknown":"\ud800","executionContext":{"workflowState":"review","latestResult":{"objectId":"result","revision":2},"acceptedReview":null,"nextSteps":[],"waiting":{"reason":"user"},"other":7}}"#,
    );
    let current = upgrade_run_record(&run).unwrap();
    assert_eq!(current.get("unknown"), run.get("unknown"));
    assert_eq!(
        current.get("executionContext"),
        Some(&parse(
            r#"{"workflowState":"review","latestResult":{"objectId":"result","revision":2},"acceptedReview":null}"#
        ))
    );
    assert_eq!(current, upgrade_run_record(&current).unwrap());
    for unchanged in [
        parse(r#"{"body":"ordinary"}"#),
        parse(r#"{"handoff":null}"#),
    ] {
        assert_eq!(upgrade_comment_record(&unchanged).unwrap(), unchanged);
    }
    for unchanged in [
        parse(r#"{"run":"r"}"#),
        parse(r#"{"executionContext":null}"#),
    ] {
        assert_eq!(upgrade_run_record(&unchanged).unwrap(), unchanged);
    }
}

#[test]
fn saved_material_uses_a_reference_count_summary_and_keeps_attachments() {
    let artifact = parse(
        r#"{"object":{"objectId":"evidence","revision":2},"label":"proof","mediaType":"application/json","byteLength":3,"contentHash":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
    );
    let mut input = task();
    input.insert("latestChecks", parse(r#"[{"note":"keep task finding"}]"#));
    input.insert("deliveries", Json::Array(vec![Json::object([
        ("result", parse(r#"{"objectId":"saved-result","revision":7}"#)),
        ("delivery", Json::object([("artifacts", Json::Array(vec![artifact.clone()])), ("codeReferences", parse(r#"[{"commit":"0123456789abcdef","path":"src/example.rs","extra":"\ud800"}]"#))])),
    ])]));
    let before = input.clone();
    let current = upgrade_task_record(&input).unwrap();
    assert!(current.get("deliveries").is_none() && current.get("latestChecks").is_none());
    let comments = current.get("comments").unwrap().as_array().unwrap();
    let saved = comments
        .iter()
        .find(|comment| {
            comment
                .get("commentId")
                .unwrap()
                .as_js_string()
                .unwrap()
                .contains_ascii("saved-material")
        })
        .unwrap();
    let body = saved.get("body").unwrap().as_js_string().unwrap();
    assert!(
        body.contains_ascii("Imported references for 1 saved results. Details remain with those result records and their attachments.")
    );
    assert!(!body.contains_ascii("0123456789abcdef") && !body.contains_ascii("src/example.rs"));
    let attachments = saved.get("attachments").unwrap().as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].get("object"), artifact.get("object"));
    assert_eq!(
        attachments[0].get("contentHash"),
        artifact.get("contentHash")
    );
    assert_eq!(current, upgrade_task_record(&current).unwrap());
    assert_eq!(input, before);
}

#[test]
fn historical_scope_fragments_preserve_text_pairs_and_existing_sequences() {
    let mut input = task();
    input.insert("comments", Json::Array(vec![question(4)]));
    let prefix = JsString::from(
        "Previous scheduling notes (context only; decide the work from the ticket):\n\n",
    );
    let mut scope = JsString(vec![b'x' as u16; 48_000 - prefix.0.len() - 1]);
    scope.0.extend([0xd83d, 0xde00, b'z' as u16]);
    input.insert(
        "nextSteps",
        Json::Array(vec![Json::object([
            ("scope", Json::String(scope.clone())),
            ("requires", Json::Array(Vec::new())),
        ])]),
    );
    let current = upgrade_task_record(&input).unwrap();
    let comments = current.get("comments").unwrap().as_array().unwrap();
    assert_eq!(comments.len(), 3);
    assert_eq!(comments[1].get("sequence"), Some(&Json::Number(5.0)));
    assert_eq!(comments[2].get("sequence"), Some(&Json::Number(6.0)));
    let mut actual = Vec::new();
    for comment in &comments[1..] {
        let units = &comment.get("body").unwrap().as_js_string().unwrap().0;
        assert!(units.len() <= 48_000);
        assert!(
            !units
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit))
        );
        actual.extend_from_slice(units);
    }
    let mut expected = prefix.0;
    expected.extend(scope.0);
    assert_eq!(actual, expected);
    assert_eq!(current, upgrade_task_record(&current).unwrap());
}
