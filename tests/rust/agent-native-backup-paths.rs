#[path = "../fixtures/reference-data.rs"]
mod reference_data;
use ivy_contracts::{
    IvyJson, IvyResult, JsString,
    json::{canonical, parse},
};
use ivy_host_runtime::{
    layout::StorageContext,
    native_backup_paths::NativeBackupPolicy,
    native_backup_policy::NativeBackupPaths,
    node_path::{Context, Style},
};
use std::{
    collections::BTreeMap,
    io::Write,
    sync::{Arc, Mutex, OnceLock},
};

fn field<'a>(value: &'a IvyJson, key: &str) -> &'a IvyJson {
    value.get(key).unwrap()
}
fn text<'a>(value: &'a IvyJson, key: &str) -> &'a JsString {
    field(value, key).as_js_string().unwrap()
}
fn map(value: &IvyJson) -> BTreeMap<JsString, JsString> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), value.as_js_string().unwrap().clone()))
        .collect()
}
fn capture(result: IvyResult<Vec<IvyJson>>) -> IvyJson {
    match result {
        Ok(rows) => IvyJson::object([("result", IvyJson::Array(rows))]),
        Err(error) => IvyJson::object([("error", error.to_wire())]),
    }
}
fn oracle() -> &'static IvyJson {
    static DATA: OnceLock<IvyJson> = OnceLock::new();
    DATA.get_or_init(|| {
        let out = reference_data::read(include_bytes!(
            "../fixtures/agent-native-backup-paths-cases.json.gz"
        ));
        parse(std::str::from_utf8(&out).unwrap(), 16 * 1024 * 1024).unwrap()
    })
}
fn wire_error(code: i32, kind: &str, message: &str) -> IvyJson {
    IvyJson::object([(
        "error",
        IvyJson::object([
            ("code", IvyJson::Number(f64::from(code))),
            ("message", message.into()),
            (
                "data",
                IvyJson::object([("code", kind.into()), ("outcome", "not_executed".into())]),
            ),
        ]),
    )])
}
fn current_contract_expected(row: &IvyJson) -> IvyJson {
    let mut expected = field(row, "expected").clone();
    if text(row, "group") == &JsString::from("settings") {
        // The frozen JavaScript oracle predates object/string admission. Invalid
        // supplied values now fail before fallback, ownership or backup effects.
        let settings = field(
            &field(field(row, "config"), "instances").as_array().unwrap()[0],
            "settings",
        );
        let message = if settings.as_object().is_none() {
            Some("Codex settings must be an object.".to_owned())
        } else {
            ["codexHome", "nativeHome"].into_iter().find_map(|name| {
                settings
                    .get(name)
                    .filter(|value| value.as_js_string().is_none())
                    .map(|_| format!("{name} must be a string when supplied."))
            })
        };
        if let Some(message) = message {
            let error = wire_error(-32602, "invalid_arguments", &message);
            return IvyJson::object([("exclusions", error.clone()), ("sharedHomes", error)]);
        }
    }
    if text(row, "platform") == &JsString::from("linux")
        && text(row, "group") == &JsString::from("shared")
        && text(row, "name")
            == &JsString::from("backslash-dotdot-name-still-checks-original-inside")
    {
        // POSIX backslashes are filename bytes. This remains a child of runtime,
        // whereas the old string-prefix oracle mistakes it for a parent escape.
        assert_eq!(
            field(
                &field(&expected, "sharedHomes")
                    .required("result")
                    .unwrap()
                    .as_array()
                    .unwrap()[0],
                "path"
            ),
            &IvyJson::from(r"/fixture/runtime/..\outside")
        );
        expected.insert("sharedHomes", wire_error(-32000, "target_conflict",
            "Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home."));
    }
    expected
}
fn compare_group(group: &str) {
    let data = oracle();
    let context = field(data, "context");
    let native_platform = JsString::from(if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    });
    let comparisons: BTreeMap<(JsString, JsString), std::cmp::Ordering> =
        field(data, "comparisons")
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let row = row.as_array().unwrap();
                (
                    (
                        row[0].as_js_string().unwrap().clone(),
                        row[1].as_js_string().unwrap().clone(),
                    ),
                    match row[2].as_f64().unwrap() as i32 {
                        -1 => std::cmp::Ordering::Less,
                        0 => std::cmp::Ordering::Equal,
                        1 => std::cmp::Ordering::Greater,
                        _ => panic!("Invalid oracle comparison"),
                    },
                )
            })
            .collect();
    let comparisons = Arc::new(comparisons);
    let mut count = 0;
    let mut current_contract_rows = 0;
    for row in field(data, "rows")
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            text(row, "group") == &JsString::from(group)
                && text(row, "platform") == &native_platform
        })
    {
        let platform = text(row, "platform").to_utf8().unwrap();
        let comparisons = comparisons.clone();
        let policy: Arc<dyn NativeBackupPaths> = Arc::new(NativeBackupPolicy::new(
            StorageContext {
                paths: Context {
                    style: if platform == "win32" {
                        Style::Windows
                    } else {
                        Style::Posix
                    },
                    cwd: field(field(context, "lexicalCwds"), &platform)
                        .as_js_string()
                        .unwrap()
                        .clone(),
                },
                account_home: text(row, "accountHome").clone(),
                compare: Arc::new(move |left, right| {
                    *comparisons
                        .get(&(left.clone(), right.clone()))
                        .expect("Comparator key must have original Node qualification")
                }),
            },
            map(field(row, "environment")),
            text(context, "processCwd").clone(),
        ));
        let config = field(row, "config");
        let actual = IvyJson::object([
            ("exclusions", capture(policy.exclusions(config))),
            ("sharedHomes", capture(policy.shared_homes(config))),
        ]);
        let expected = current_contract_expected(row);
        current_contract_rows += usize::from(&expected != field(row, "expected"));
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(&expected, usize::MAX).unwrap(),
            "{platform} {group}: {}",
            text(row, "name").to_utf8().unwrap()
        );
        if let Some(path) = std::env::var_os("IVY_BACKUP_PATHS_PROOF") {
            static PROOF_WRITE: Mutex<()> = Mutex::new(());
            let _write = PROOF_WRITE.lock().unwrap();
            let proof = IvyJson::object([
                ("platform", field(row, "platform").clone()),
                ("group", field(row, "group").clone()),
                ("name", field(row, "name").clone()),
                ("actual", actual),
                ("expected", expected),
                ("frozenExpected", field(row, "expected").clone()),
            ]);
            writeln!(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap(),
                "{}",
                canonical(&proof, usize::MAX).unwrap()
            )
            .unwrap();
        }
        count += 1;
    }
    assert!(count > 0, "No native host fixtures selected");
    assert_eq!(
        current_contract_rows,
        match group {
            "settings" => 20,
            "shared" if cfg!(target_os = "linux") => 1,
            _ => 0,
        },
        "Only the explicitly superseded oracle expectations may differ"
    );
}
#[test]
fn explicit_ownership_and_storage_keys() {
    compare_group("ownership");
}
#[test]
fn shared_homes_and_symmetric_overlap() {
    compare_group("shared");
}
#[test]
fn raw_settings_and_home_resolution() {
    compare_group("settings");
}
#[test]
fn original_branch_and_error_order() {
    compare_group("errors");
}
#[test]
fn required_locale_comparison_and_stable_results() {
    compare_group("sort");
}

#[test]
fn shared_home_overlap_uses_native_components_and_keeps_references_verbatim() {
    use ivy_host_runtime::native_path;
    let base = std::env::temp_dir().join(format!("ivy-backup-path-{}", uuid::Uuid::new_v4()));
    assert!(!base.exists());
    let path = |relative: &str| IvyJson::String(native_path::from_native(&base.join(relative)));
    let policy = NativeBackupPolicy::new(
        StorageContext {
            paths: Context {
                style: Style::native(),
                cwd: native_path::from_native(&base),
            },
            account_home: native_path::from_native(&base.join("account")),
            compare: Arc::new(|_, _| panic!("A single reference needs no comparison")),
        },
        BTreeMap::new(),
        native_path::from_native(&base),
    );
    for (relative, conflict) in [
        ("", true),
        ("runtime", true),
        ("runtime/child/../leaf", true),
        ("runtime/child/..", true),
        ("runtime/../outside", false),
        ("runtime/child/../../outside", false),
        ("runtime-sibling", false),
        (r"runtime/..\outside", cfg!(unix)),
        (r"runtime/.\outside", true),
        (r"runtime\outside", cfg!(windows)),
        (r"runtime\", cfg!(windows)),
    ] {
        let selected = path(relative);
        let config = IvyJson::object([
            ("runtimeRoot", path("runtime")),
            ("artifactRoot", path("software")),
            ("stagingRoot", path("staging")),
            (
                "instances",
                IvyJson::Array(vec![IvyJson::object([
                    ("instanceId", "agent".into()),
                    ("componentId", "agent-manager".into()),
                    (
                        "settings",
                        IvyJson::object([(
                            "appServer",
                            IvyJson::object([
                                ("mode", "external-proxy".into()),
                                ("expectedCodexHome", selected.clone()),
                            ]),
                        )]),
                    ),
                ])]),
            ),
        ]);
        assert!(policy.exclusions(&config).unwrap().is_empty(), "{relative}");
        let actual = capture(policy.shared_homes(&config));
        let expected = if conflict {
            wire_error(
                -32000,
                "target_conflict",
                "Shared Codex homes must not overlap owned host, service, project or software storage. Use explicit owned-stdio for an instance-owned home.",
            )
        } else {
            IvyJson::object([(
                "result",
                IvyJson::Array(vec![IvyJson::object([
                    ("instanceId", "agent".into()),
                    ("kind", "external-user-state".into()),
                    ("path", selected),
                ])]),
            )])
        };
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(&expected, usize::MAX).unwrap(),
            "{relative}"
        );
    }
    assert!(
        !base.exists(),
        "Backup path decisions must not touch shared homes"
    );
}
