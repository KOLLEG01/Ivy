use ivy_contracts::{
    IvyJson as J,
    error::{JavaScriptException, JavaScriptExceptionKind},
    hash::digest,
    json,
};
use ivy_sdk::{
    upgrade_configuration_record, upgrade_result_content, upgrade_result_record,
    upgrade_task_record,
};
use std::{borrow::Cow, path::Path};

fn fixture() -> J {
    json::parse(
        include_str!("../fixtures/task-board-records.json"),
        usize::MAX,
    )
    .unwrap()
}
fn error(error: JavaScriptException) -> J {
    J::object([(
        "error",
        J::object([
            (
                "kind",
                match error.kind {
                    JavaScriptExceptionKind::TypeError => "TypeError",
                    JavaScriptExceptionKind::RangeError => "RangeError",
                }
                .into(),
            ),
            ("message", J::String(error.message)),
        ]),
    )])
}
fn observe(helper: &str, value: &J) -> J {
    let result = match helper {
        "upgradeRequirement" => {
            Ok(ivy_sdk::task_board_records::upgrade_requirement(Some(value)).unwrap())
        }
        "upgradeConfigurationRecord" => upgrade_configuration_record(value),
        "upgradeTaskRecord" => upgrade_task_record(value).map(Cow::Owned),
        "upgradeResultContent" => upgrade_result_content(value).map(Cow::Owned),
        "upgradeResultRecord" => upgrade_result_record(value).map(Cow::Owned),
        _ => panic!("unknown fixture helper"),
    };
    match result {
        Ok(value) => {
            let borrowed = matches!(value, Cow::Borrowed(_));
            J::object([
                ("value", value.into_owned()),
                ("sameIdentity", borrowed.into()),
            ])
        }
        Err(e) => error(e),
    }
}
fn evidence(name: &str, value: &J) {
    if let Some(dir) = std::env::var_os("IVY_TEST_RAW_EVIDENCE") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Path::new(&dir).join(name),
            json::stringify(value, usize::MAX).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn all_seventy_retained_original_record_boundaries_preserve_values_order_and_raw_failures() {
    let fixture = fixture();
    assert_eq!(
        fixture.get("sourceHashes").unwrap().get("records"),
        Some(&digest(include_bytes!("../fixtures/task-board-records.ts.txt")).into())
    );
    let rows = fixture.get("cases").unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 70);
    let mut observations = Vec::new();
    let mut failures = Vec::new();
    for row in rows {
        let input = row.get("input").unwrap();
        let before = json::stringify(input, usize::MAX).unwrap();
        let helper = row.get("helper").unwrap().as_str().unwrap();
        let actual = observe(&helper, input);
        let mut expected = row.get("expected").unwrap().clone();
        expected.remove("outerError");
        let actual_text = json::stringify(&actual, usize::MAX).unwrap();
        let expected_text = json::stringify(&expected, usize::MAX).unwrap();
        let matches = actual_text == expected_text;
        if !matches {
            let first = actual_text
                .bytes()
                .zip(expected_text.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(actual_text.len().min(expected_text.len()));
            failures.push(format!(
                "{}: first byte {first}; actual {}, expected {}",
                row.get("name").unwrap().as_str().unwrap(),
                digest(actual_text.as_bytes()),
                digest(expected_text.as_bytes())
            ));
        }
        observations.push(J::object([
            ("name", row.get("name").unwrap().clone()),
            ("actual", actual),
            ("expected", expected),
            ("matches", matches.into()),
        ]));
        assert_eq!(json::stringify(input, usize::MAX).unwrap(), before);
    }
    evidence("task-board-records-native.json", &J::Array(observations));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn exact_public_signatures_keep_no_op_borrows_and_raw_cause_until_the_owner_catch() {
    let null = J::Null;
    assert!(ivy_sdk::task_board_records::upgrade_requirement(None).is_none());
    assert!(
        matches!(ivy_sdk::task_board_records::upgrade_requirement(Some(&null)), Some(Cow::Borrowed(v)) if std::ptr::eq(v, &null))
    );
    let configuration = json::parse(
        r#"{"defaults":{"executionRequirement":null},"unknown":"\ud800"}"#,
        usize::MAX,
    )
    .unwrap();
    assert!(
        matches!(upgrade_configuration_record(&configuration).unwrap(), Cow::Borrowed(v) if std::ptr::eq(v, &configuration))
    );
    type Upgrade = fn(&J) -> Result<J, JavaScriptException>;
    let task: Upgrade = upgrade_task_record;
    let result: Upgrade = upgrade_result_record;
    let content: Upgrade = upgrade_result_content;
    let raw = task(&null).unwrap_err();
    assert_eq!(raw.kind, JavaScriptExceptionKind::TypeError);
    assert_eq!(
        raw.message,
        "Cannot destructure property 'latestChecks' of 'task' as it is null.".into()
    );
    assert!(std::error::Error::source(&raw).is_none());
    assert_eq!(
        result(&J::object::<&str>([])).unwrap_err().kind,
        JavaScriptExceptionKind::TypeError
    );
    assert_eq!(
        content(&null).unwrap_err().kind,
        JavaScriptExceptionKind::TypeError
    );
}
