use super::*;
use ivy_contracts::JsString;
use ivy_hive::storage_inspector::inspect_admitted_configuration;
use ivy_host_runtime::native_path;
use std::collections::BTreeMap;

fn config(root: &Path) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("hostId", "host".into()),
        ("instanceId", "hive".into()),
        ("serviceNodeId", "hive".into()),
        ("componentId", "hive".into()),
        ("publicBaseUrl", "https://example.invalid".into()),
        ("dataRoot", native_path::from_native(root).into()),
        (
            "artifactRoot",
            native_path::from_native(&root.with_extension("artifact")).into(),
        ),
        ("buildId", format!("sha256:{}", "a".repeat(64)).into()),
        ("version", "1.0.0".into()),
        ("settings", IvyJson::object(Vec::<(String, IvyJson)>::new())),
    ])
}
fn invoke(config: &IvyJson, args: &[String]) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ivy-hive-inspect-storage"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(config, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    json!({"status":out.status.code(),"stdout":String::from_utf8(out.stdout).unwrap(),"stderr":String::from_utf8(out.stderr).unwrap()})
}
fn domain(key: impl Into<JsString>, versions: &[&str]) -> IvyJson {
    IvyJson::object([
        ("key", IvyJson::String(key.into())),
        (
            "versions",
            IvyJson::Array(versions.iter().map(|v| (*v).into()).collect()),
        ),
    ])
}
fn report(exists: bool, contracts: Vec<IvyJson>) -> IvyJson {
    IvyJson::object([
        ("schemaVersion", 1.0.into()),
        ("exists", exists.into()),
        ("format", (if exists { 4.0 } else { 0.0 }).into()),
        ("contracts", IvyJson::Array(contracts)),
    ])
}
fn args(requirements: Value) -> Vec<String> {
    vec!["--contracts".into(), requirements.to_string()]
}
fn assert_result(actual: Value, expected: Result<IvyJson, &str>) {
    match expected {
        Ok(value) => {
            assert_eq!(actual["status"], 0, "{actual}");
            assert_eq!(actual["stderr"], "");
            let stdout = actual["stdout"].as_str().unwrap();
            assert_eq!(stdout.lines().count(), 1);
            assert!(stdout.ends_with('\n'));
            assert_eq!(parse(stdout, usize::MAX).unwrap(), value);
        }
        Err(code) => {
            assert_eq!(actual["status"], 1, "{actual}");
            assert_eq!(actual["stdout"], "");
            let stderr = actual["stderr"].as_str().unwrap();
            assert_eq!(stderr.lines().count(), 1);
            assert!(stderr.ends_with('\n'));
            assert_eq!(
                serde_json::from_str::<Value>(stderr).unwrap(),
                json!({"code":code,"message":"Hive storage inspection failed without mutation."})
            );
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}
fn image(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut image = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let entry = if metadata.file_type().is_symlink() {
            Entry::Link(fs::read_link(&path).unwrap())
        } else if metadata.is_file() {
            Entry::File(fs::read(&path).unwrap())
        } else {
            pending.extend(fs::read_dir(&path).unwrap().map(|v| v.unwrap().path()));
            Entry::Directory
        };
        image.insert(path.strip_prefix(root).unwrap().to_owned(), entry);
    }
    image
}
fn relative_to_current(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let from: Vec<_> = cwd.components().collect();
    let to: Vec<_> = path.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    assert!(common > 0);
    let mut result: PathBuf = (common..from.len()).map(|_| "..").collect();
    for part in &to[common..] {
        result.push(part.as_os_str());
    }
    result
}
fn directory_link(target: &Path, link: &Path) {
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn native_admitted_offline_inspector_keeps_arguments_domains_errors_and_missing_output_validation()
{
    let root = Root::new();
    let path = root.0.join("hive.sqlite");
    seed(&path);
    let before = image(&root.0);
    let configuration = config(&root.0);
    let all = ["0.1.0", "1.0.0", "1.1.0", "2.0.0", "2.1.0", "3.0.0"];
    let mut cases = vec![(vec![], Ok(report(true, vec![])))];
    for (requirements, expected) in [
        (
            json!([{"key":"fixture/value"}]),
            Ok(report(true, vec![domain("fixture/value", &all)])),
        ),
        (
            json!([{"key":"fixture/value","readScope":{"roots":["root"],"history":"current","includeArchived":false,"references":[{"objectId":"archived","revision":1}]}}]),
            Ok(report(
                true,
                vec![domain("fixture/value", &["0.1.0", "2.0.0", "2.1.0"])],
            )),
        ),
        (json!([{"key":"fixture/many"}]), Err("limit_exceeded")),
        (
            json!([{"key":"fixture/value"},{"key":"fixture/value"}]),
            Err("invalid_arguments"),
        ),
        (json!([{}]), Err("invalid_arguments")),
        (json!([null]), Err("internal_error")),
        (json!({}), Err("invalid_arguments")),
        (json!([{"key":"x".repeat(193)}]), Err("invalid_arguments")),
        (
            json!([{"key":"🙂".repeat(96)}]),
            Ok(report(true, vec![domain("🙂".repeat(96), &[])])),
        ),
    ] {
        cases.push((args(requirements), expected));
    }
    cases.extend([
        (
            vec!["--contracts".into(), "[".into()],
            Err("internal_error"),
        ),
        (vec!["--contracts".into()], Err("internal_error")),
        (
            vec!["--contracts".into(), "[{\"key\":\"\\ud800\"}]".into()],
            Ok(report(true, vec![domain(JsString(vec![0xd800]), &[])])),
        ),
        (
            vec![
                "ignored".into(),
                "--contracts".into(),
                "[]".into(),
                "--contracts".into(),
                "[".into(),
            ],
            Ok(report(true, vec![])),
        ),
    ]);
    assert_eq!(cases.len(), 14);
    for (args, expected) in cases {
        assert_result(invoke(&configuration, &args), expected);
    }
    let mut other = configuration.clone();
    other.insert("componentId", "fixture".into());
    assert_result(
        invoke(&other, &["--contracts".into(), "[".into()]),
        Err("target_conflict"),
    );
    let mut invalid = other.clone();
    invalid.insert("schemaVersion", 2.0.into());
    assert_result(
        invoke(&invalid, &["--contracts".into(), "[".into()]),
        Err("invalid_arguments"),
    );
    let missing = config(&root.0.join("missing"));
    for (requirements, expected) in [
        (
            json!([{"key":"fixture/value"}]),
            Ok(report(false, vec![domain("fixture/value", &[])])),
        ),
        (json!([{}]), Err("invalid_arguments")),
        (json!([null]), Err("internal_error")),
    ] {
        assert_result(invoke(&missing, &args(requirements)), expected);
    }
    assert_eq!(image(&root.0), before);

    let mut variants = vec![
        "unicode",
        "dot",
        "parent",
        "relative",
        "canonical",
        "linked",
        "link-parent",
    ];
    #[cfg(windows)]
    variants.extend(["trailing-name", "opaque-high", "opaque-low"]);
    for variant in variants {
        let root = Root::new();
        let namespace = fs::canonicalize(&root.0).unwrap();
        let mut data = root.0.join(if variant == "unicode" {
            "data-漢字 space"
        } else {
            "data"
        });
        #[cfg(windows)]
        if variant == "trailing-name" {
            data = namespace.join("data. ");
            fs::create_dir(namespace.join("data")).unwrap();
            fs::write(
                namespace.join("data/hive.sqlite"),
                b"untouched trailing alias",
            )
            .unwrap();
        } else if variant.starts_with("opaque-") {
            use std::{ffi::OsString, os::windows::ffi::OsStringExt};
            let mut name: Vec<_> = "data".encode_utf16().collect();
            name.push(if variant == "opaque-high" {
                0xd800
            } else {
                0xdfff
            });
            data = namespace.join(OsString::from_wide(&name));
            for alias in ["data�", "data. "] {
                fs::create_dir(namespace.join(alias)).unwrap();
                fs::write(
                    namespace.join(alias).join("hive.sqlite"),
                    b"untouched native alias",
                )
                .unwrap();
            }
        }
        fs::create_dir(&data).unwrap();
        fs::write(data.join("hive.sqlite"), fs::read(&path).unwrap()).unwrap();
        let configured = match variant {
            "dot" => root.0.join(".").join("data"),
            "parent" => root.0.join("absent").join("..").join("data"),
            "relative" => relative_to_current(&data),
            "canonical" => fs::canonicalize(&data).unwrap(),
            "linked" => {
                let link = root.0.join("linked");
                directory_link(&data, &link);
                link
            }
            "link-parent" => {
                fs::create_dir_all(root.0.join("elsewhere/child")).unwrap();
                fs::create_dir(root.0.join("elsewhere/data")).unwrap();
                let alias = root.0.join("elsewhere/data/hive.sqlite");
                seed(&alias);
                let db = Connection::open(&alias).unwrap();
                db.pragma_update(None, "user_version", 3).unwrap();
                drop(db);
                let link = root.0.join("linked");
                directory_link(&root.0.join("elsewhere/child"), &link);
                link.join("..").join("data")
            }
            _ => data,
        };
        let before = image(&namespace);
        let expected = if variant.starts_with("opaque-") {
            Err("internal_error")
        } else {
            Ok(report(true, vec![domain("fixture/value", &all)]))
        };
        let configuration = config(&configured);
        assert_result(
            invoke(&configuration, &args(json!([{"key":"fixture/value"}]))),
            expected.clone(),
        );
        let direct_args = args(json!([{"key":"fixture/value"}]))
            .into_iter()
            .map(JsString::from)
            .collect::<Vec<_>>();
        let direct = inspect_admitted_configuration(&configuration, &direct_args);
        match expected {
            Ok(value) => assert_eq!(direct.unwrap(), value, "{variant}"),
            Err(code) => assert_eq!(direct.unwrap_err().code, code, "{variant}"),
        }
        assert_eq!(
            image(&namespace),
            before,
            "{variant}: read-only native tree and aliases"
        );
    }
}

#[test]
fn native_admitted_offline_inspector_obeys_stopped_writer_lock_timeout_and_format_without_mutation()
{
    let root = Root::new();
    let path = root.0.join("hive.sqlite");
    seed(&path);
    let configuration = config(&root.0);
    let args = args(json!([{"key":"fixture/value"}]));
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
        .unwrap();
    assert_result(invoke(&configuration, &args), Err("internal_error"));
    db.execute_batch("ROLLBACK;").unwrap();
    drop(db);
    let before = image(&root.0);
    assert_result(
        invoke(&configuration, &args),
        Ok(report(
            true,
            vec![domain(
                "fixture/value",
                &["0.1.0", "1.0.0", "1.1.0", "2.0.0", "2.1.0", "3.0.0"],
            )],
        )),
    );
    assert_eq!(image(&root.0), before);
    let db = Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 3).unwrap();
    drop(db);
    let before = image(&root.0);
    assert_result(
        invoke(&configuration, &args),
        Err("unsupported_storage_format"),
    );
    assert_eq!(image(&root.0), before);
}
