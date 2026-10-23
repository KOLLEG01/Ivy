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
    path::PathBuf,
    process::Command,
    sync::{Arc, OnceLock},
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
        let out = Command::new("node")
            .arg("tests/fixtures/agent-native-backup-paths-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        parse(std::str::from_utf8(&out.stdout).unwrap(), 16 * 1024 * 1024).unwrap()
    })
}
fn compare_group(group: &str) {
    let data = oracle();
    let context = field(data, "context");
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
    for row in field(data, "rows")
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            text(row, "group") == &JsString::from(group)
                && text(row, "platform") == text(context, "nativePlatform")
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
        let expected = field(row, "expected");
        assert_eq!(
            canonical(&actual, usize::MAX).unwrap(),
            canonical(expected, usize::MAX).unwrap(),
            "{platform} {group}: {}",
            text(row, "name").to_utf8().unwrap()
        );
        if let Some(path) = std::env::var_os("IVY_BACKUP_PATHS_PROOF") {
            let proof = IvyJson::object([
                ("platform", field(row, "platform").clone()),
                ("group", field(row, "group").clone()),
                ("name", field(row, "name").clone()),
                ("actual", actual),
                ("expected", expected.clone()),
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
