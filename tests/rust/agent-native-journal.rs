use ivy_agent_manager::native_journal::{Identity, Limits, NativeJournal, Owner};
use ivy_contracts::{
    IvyError, IvyJson, IvyResult, JsString, Outcome,
    hash::hash_json,
    json::{canonical, parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use std::{
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

fn data() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let out = Command::new("node")
            .arg("tests/fixtures/agent-native-journal-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        parse(std::str::from_utf8(&out.stdout).unwrap(), 4 * 1024 * 1024).unwrap()
    })
}
fn field<'a>(v: &'a IvyJson, k: &str) -> &'a IvyJson {
    v.get(k).unwrap()
}
fn text<'a>(v: &'a IvyJson, k: &str) -> &'a JsString {
    field(v, k).as_js_string().unwrap()
}
fn number(v: &IvyJson, k: &str) -> usize {
    field(v, k).as_f64().unwrap() as usize
}
fn identity(v: &IvyJson) -> Identity {
    Identity {
        caller_principal_id: text(v, "callerPrincipalId").clone(),
        operation_id: text(v, "operationId").clone(),
    }
}
fn capture(v: &IvyJson) -> IvyResult<IvyJson> {
    let bytes = stringify(v, MANAGEMENT_FRAME_BYTES)?.len();
    Ok(if bytes > 4096 {
        IvyJson::object([
            ("encodedBytes", IvyJson::Number(bytes as f64)),
            ("hash", hash_json(v)?.into()),
        ])
    } else {
        IvyJson::object([("value", v.clone())])
    })
}
fn materialize(v: &IvyJson) -> IvyJson {
    match v {
        IvyJson::Object(entries) => {
            if let Some(value) = v.get("$fill") {
                return IvyJson::String(JsString(
                    value.as_js_string().unwrap().0.repeat(number(v, "count")),
                ));
            }
            IvyJson::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), materialize(value)))
                    .collect(),
            )
        }
        IvyJson::Array(entries) => IvyJson::Array(entries.iter().map(materialize).collect()),
        _ => v.clone(),
    }
}
fn set(value: &mut IvyJson, key: &str, replacement: IvyJson) {
    let IvyJson::Object(entries) = value else {
        panic!()
    };
    let key = JsString::from(key);
    if let Some((_, value)) = entries.iter_mut().find(|(k, _)| k == &key) {
        *value = replacement;
    } else {
        entries.push((key, replacement));
    }
}
fn execute(
    journal: &mut NativeJournal,
    c: &IvyJson,
    callbacks: &Arc<Mutex<Vec<IvyJson>>>,
) -> IvyResult<IvyJson> {
    let action = text(c, "action").to_utf8().unwrap();
    match action.as_str() {
        "accept" => {
            let (value, created) = journal.accept(
                &identity(field(c, "identity")),
                text(c, "method"),
                field(c, "params"),
            )?;
            Ok(IvyJson::object([
                ("operation", value),
                ("created", created.into()),
            ]))
        }
        "prevent" => journal.prevent(
            &identity(field(c, "identity")),
            text(c, "method"),
            field(c, "params"),
        ),
        "beginEpoch" => journal.begin_epoch(text(c, "epoch")).map(|_| IvyJson::Null),
        "loseEpoch" => journal
            .lose_epoch(text(c, "epoch"), text(c, "code"))
            .map(|_| IvyJson::Null),
        "dispatch" => journal.dispatch(
            &identity(field(c, "identity")),
            text(c, "epoch"),
            field(c, "requestId"),
        ),
        "finish" => journal.finish(
            &identity(field(c, "identity")),
            text(c, "epoch"),
            field(c, "requestId"),
            field(c, "reply"),
        ),
        "rejectBeforeDispatch" => {
            journal.reject_before_dispatch(&identity(field(c, "identity")), text(c, "code"))
        }
        "get" => journal
            .get(&identity(field(c, "identity")))
            .map(|v| v.unwrap_or(IvyJson::Null)),
        "forget" => journal
            .forget(&identity(field(c, "identity")))
            .map(|_| IvyJson::Null),
        "retainedInHive" => journal
            .retained_in_hive(&identity(field(c, "identity")))
            .map(|_| IvyJson::Null),
        "observeInput" => {
            journal.observe_input(field(c, "identity"), text(c, "method"), field(c, "params"))
        }
        "input" => journal
            .input(field(c, "identity"))
            .map(|v| v.unwrap_or(IvyJson::Null)),
        "dispatchAnswer" => journal.dispatch_answer(
            &identity(field(c, "operation")),
            field(c, "identity"),
            field(c, "reply"),
        ),
        "resolveInput" => journal
            .resolve_input(text(c, "epoch"), field(c, "requestId"), text(c, "threadId"))
            .map(|v| v.unwrap_or(IvyJson::Null)),
        "inputs" => journal.inputs(field(c, "query")),
        "observedMethods" => Ok(journal.observed_methods()),
        "reserveNotificationSequence" => Ok(journal.reserve_notification_sequence()),
        "inventoryRevision" => Ok(IvyJson::Number(journal.inventory_revision(
            &text(c, "kind").to_utf8().unwrap(),
            field(c, "minimum").as_f64().unwrap(),
        ))),
        "close" => {
            journal.close();
            Ok(IvyJson::Null)
        }
        "mutateRetrieved" => {
            let mut value = journal.get(&identity(field(c, "identity")))?.unwrap();
            set(
                &mut value,
                "params",
                IvyJson::object([("foreign", true.into())]),
            );
            set(&mut value, "phase", "foreign".into());
            Ok(IvyJson::Null)
        }
        "callback" => {
            let callbacks = callbacks.clone();
            let mode = text(c, "mode").clone();
            journal.set_on_terminal(Some(Box::new(move |value| {
                callbacks.lock().unwrap().push(capture(value)?);
                if mode == JsString::from("mutate") {
                    set(value, "callbackTouched", true.into());
                }
                if mode == JsString::from("throw") {
                    return Err(
                        IvyError::new("fixture_callback", "fixture callback failure")
                            .with_outcome(Outcome::Unknown),
                    );
                }
                Ok(())
            })));
            Ok(IvyJson::Null)
        }
        "sweepObservations" => {
            for i in 0..number(c, "count") {
                let id = Identity {
                    caller_principal_id: "caller".into(),
                    operation_id: format!("sweep-{i}").into(),
                };
                journal.accept(
                    &id,
                    &format!("method/{i}").into(),
                    &IvyJson::object([] as [(&str, IvyJson); 0]),
                )?;
                journal.reject_before_dispatch(&id, &"refused".into())?;
                journal.retained_in_hive(&id)?;
            }
            Ok(field(c, "count").clone())
        }
        _ => panic!("Unknown fixture action {action}"),
    }
}
fn equal(actual: &IvyJson, expected: &IvyJson, label: &str) {
    assert_eq!(
        canonical(actual, MANAGEMENT_FRAME_BYTES).unwrap(),
        canonical(expected, MANAGEMENT_FRAME_BYTES).unwrap(),
        "{label}"
    );
}
fn run(large: bool) {
    let base = field(data(), "base").as_f64().unwrap() as i64;
    let cases = field(data(), "cases").as_array().unwrap();
    assert_eq!(cases.len(), 15);
    let mut selected = 0;
    for row in cases {
        let name = text(row, "name").to_utf8().unwrap();
        let is_large = [
            "full-request-and-reply",
            "maximum-input-and-answer",
            "page-budget",
        ]
        .contains(&name.as_str());
        if large != is_large {
            continue;
        }
        selected += 1;
        let owner = field(row, "owner");
        let limits = field(row, "limits");
        let tick = Arc::new(AtomicU64::new(0));
        let clock = tick.clone();
        let journal = NativeJournal::with_clock(
            Owner {
                service_node_id: text(owner, "serviceNodeId").clone(),
                native_version: text(owner, "nativeVersion").clone(),
                native_executable_hash: text(owner, "nativeExecutableHash").clone(),
            },
            Limits {
                max_operations: number(limits, "maxOperations"),
                max_journal_bytes: number(limits, "maxJournalBytes"),
                max_pending_inputs: number(limits, "maxPendingInputs"),
            },
            Arc::new(move || {
                chrono::DateTime::from_timestamp_millis(
                    base + clock.fetch_add(1, Ordering::SeqCst) as i64,
                )
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into()
            }),
        );
        if let Some(expected) = row.get("constructorError") {
            equal(
                &journal.err().expect("Constructor should fail").to_wire(),
                expected,
                &name,
            );
            continue;
        }
        let mut journal = journal.unwrap();
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        for (index, (descriptor, expected)) in field(row, "commands")
            .as_array()
            .unwrap()
            .iter()
            .zip(field(row, "expected").as_array().unwrap())
            .enumerate()
        {
            let command = materialize(descriptor);
            let result = execute(&mut journal, &command, &callbacks).and_then(|v| capture(&v));
            let outcome = match result {
                Ok(value) => IvyJson::object([("result", value)]),
                Err(e) => IvyJson::object([("error", e.to_wire())]),
            };
            let actual = IvyJson::object([
                ("outcome", outcome),
                ("status", journal.status()),
                (
                    "epoch",
                    journal
                        .epoch()
                        .cloned()
                        .map(IvyJson::String)
                        .unwrap_or(IvyJson::Null),
                ),
                (
                    "pending",
                    IvyJson::Number(journal.pending_input_count() as f64),
                ),
                ("observed", capture(&journal.observed_methods()).unwrap()),
                (
                    "callbacks",
                    capture(&IvyJson::Array(callbacks.lock().unwrap().clone())).unwrap(),
                ),
                ("tick", IvyJson::Number(tick.load(Ordering::SeqCst) as f64)),
            ]);
            equal(
                &actual,
                expected,
                &format!(
                    "{name} step{index} {}",
                    text(&command, "action").to_utf8().unwrap()
                ),
            );
        }
    }
    assert_eq!(selected, if large { 3 } else { 12 });
}
#[test]
fn native_journal_transitions_inputs_and_admission_match_original() {
    run(false);
}
#[test]
fn native_journal_full_frames_and_management_pages_match_original() {
    run(true);
}
