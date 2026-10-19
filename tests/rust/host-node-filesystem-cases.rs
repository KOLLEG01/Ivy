use super::{ServiceFixture, assert_json_eq};
use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_runtime::node_path::{self, Context, Style};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};
static CORPUS: Mutex<()> = Mutex::new(());
fn call(input: &IvyJson) -> IvyJson {
    let mut p = Command::new("node")
        .arg("tests/fixtures/host-node-filesystem-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    p.stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let out = p.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    json::parse(std::str::from_utf8(&out.stdout).unwrap(), usize::MAX).unwrap()
}
fn pair(case: &str, actual: IvyJson, expected: IvyJson) {
    if let Some(file) = std::env::var_os("IVY_HOST_NODE_FILESYSTEM_CORPUS") {
        let _guard = CORPUS.lock().unwrap();
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file)
                .unwrap(),
            "{}",
            json::stringify(
                &IvyJson::object([
                    ("case", case.into()),
                    ("actual", actual.clone()),
                    ("expected", expected.clone())
                ]),
                usize::MAX
            )
            .unwrap()
        )
        .unwrap();
    }
    assert_json_eq(&actual, &expected);
}

#[test]
fn filesystem_mapping_matches_node_dos_unc_verbatim_long_and_utf8_boundaries() {
    let values: Vec<JsString> = vec![
        "".into(),
        ".".into(),
        "file-..".into(),
        "file- ".into(),
        "..\\file-..".into(),
        "C:".into(),
        "C:file-..".into(),
        "D:file- ".into(),
        "\\file-..".into(),
        "C:\\owned\\file-..".into(),
        "C:/owned/../file- ".into(),
        "C:\\owned\\file-..\\".into(),
        "\\\\server\\share\\file-..".into(),
        "//server/share/file- ".into(),
        "\\\\?\\C:\\owned\\a\\..\\file-..".into(),
        "\\\\?\\UNC\\server\\share\\file- ".into(),
        "\\\\.\\pipe\\named".into(),
        "/owned/file-..".into(),
        JsString(vec![67, 58, 92, 0xd800, 92, 102, 105, 108, 101, 45, 46, 46]),
        format!("C:\\{}file-..", "long-segment\\".repeat(32)).into(),
    ];
    let mut cases = Vec::new();
    let mut actual = Vec::new();
    for style in [Style::Windows, Style::Posix] {
        let context = Context {
            style,
            cwd: if style == Style::Windows {
                "C:\\work\\current".into()
            } else {
                "/work/current".into()
            },
            drive_cwds: BTreeMap::from([
                ("C:".into(), "C:\\drive\\current".into()),
                ("D:".into(), "D:\\other\\current".into()),
            ]),
        };
        for value in &values {
            let path = context.filesystem_path(value);
            actual.push(IvyJson::String(node_path::from_native(&path)));
            cases.push(IvyJson::object([
                (
                    "style",
                    if style == Style::Windows {
                        "win32".into()
                    } else {
                        "posix".into()
                    },
                ),
                ("cwd", IvyJson::String(context.cwd.clone())),
                (
                    "drives",
                    IvyJson::object(
                        context
                            .drive_cwds
                            .iter()
                            .map(|(k, v)| (k.clone(), IvyJson::String(v.clone()))),
                    ),
                ),
                ("path", IvyJson::String(value.clone())),
            ]));
        }
    }
    pair(
        "namespace-matrix",
        IvyJson::Array(actual),
        call(&IvyJson::object([
            ("operation", "paths".into()),
            ("cases", IvyJson::Array(cases)),
        ])),
    );
}

fn scan(root: &Path, prefix: JsString, values: &mut Vec<(JsString, IvyJson)>) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let name = node_path::node_utf8(&entry.file_name());
        let mut relative = prefix.0.clone();
        relative.extend(name.0);
        let relative = JsString(relative);
        if entry.file_type().unwrap().is_dir() {
            let mut prefix = relative.0.clone();
            prefix.push(47);
            scan(&entry.path(), JsString(prefix), values);
        } else {
            let bytes = fs::read(entry.path()).unwrap();
            values.push((
                relative.clone(),
                IvyJson::Array(vec![
                    IvyJson::String(relative),
                    IvyJson::Number(bytes.len() as f64),
                    Sha256::digest(&bytes)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                        .into(),
                ]),
            ));
        }
    }
}

#[test]
fn actual_node_and_host_file_creation_preserve_trailing_dot_space_verbatim_and_long_names() {
    let fixture = ServiceFixture::new();
    actual_file_creation(&fixture, "actual-node-filesystem");
}

#[cfg(windows)]
#[test]
#[ignore = "requires an existing explicitly supplied writable UNC test root"]
fn actual_unc_file_creation_matches_node_without_provisioning_a_share() {
    let base = std::env::var_os("IVY_HOST_UNC_TEST_ROOT").expect("explicit UNC test root");
    let base = PathBuf::from(base);
    assert!(node_path::from_native(&base).0.starts_with(&[92, 92]));
    let fixture = ServiceFixture(base.join(format!("ivy-filesystem-{}", uuid::Uuid::new_v4())));
    fs::create_dir_all(
        node_path::node_filesystem_path(&node_path::from_native(&fixture.0)).unwrap(),
    )
    .unwrap();
    actual_file_creation(&fixture, "actual-node-unc-filesystem");
}

fn actual_file_creation(fixture: &ServiceFixture, case_name: &str) {
    let root = fixture.0.join("native");
    let mut cases = vec![
        IvyJson::object([("path", "file-..".into())]),
        IvyJson::object([("path", "file- ".into())]),
        IvyJson::object([("path", "file- . ".into())]),
        IvyJson::object([("path", "nested-../leaf-..".into())]),
        IvyJson::object([("path", "forward/file-..".into()), ("forward", true.into())]),
        IvyJson::object([(
            "path",
            IvyJson::String(JsString(vec![117, 110, 105, 99, 111, 100, 101, 45, 0xd800])),
        )]),
        IvyJson::object([(
            "path",
            format!("{}file-..", "long-segment-0123456789/".repeat(18)).into(),
        )]),
    ];
    #[cfg(windows)]
    cases.push(IvyJson::object([
        ("path", "verbatim/file- ".into()),
        ("verbatim", true.into()),
    ]));
    let root_js = node_path::from_native(&root);
    for case in &cases {
        let mut logical = node_path::join(
            Style::native(),
            &[
                root_js.clone(),
                case.required("path")
                    .unwrap()
                    .as_js_string()
                    .unwrap()
                    .clone(),
            ],
        );
        if case.get("forward") == Some(&true.into()) {
            for unit in &mut logical.0 {
                if *unit == 92 {
                    *unit = 47
                }
            }
        }
        if case.get("verbatim") == Some(&true.into()) {
            logical =
                node_path::from_native(&Context::current().unwrap().filesystem_path(&logical));
        }
        let saved = logical.clone();
        let physical = node_path::node_filesystem_path(&logical).unwrap();
        fs::create_dir_all(physical.parent().unwrap()).unwrap();
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&physical)
            .unwrap();
        file.write_all(&[1, 2, 3]).unwrap();
        drop(file);
        assert_eq!(fs::read(&physical).unwrap(), [1, 2, 3]);
        assert_eq!(logical, saved);
        assert_eq!(node_path::filesystem_path(&logical), physical);
    }
    let physical = node_path::node_filesystem_path(&root_js).unwrap();
    let mut values = Vec::new();
    scan(&physical, JsString::default(), &mut values);
    values.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let actual = IvyJson::Array(values.into_iter().map(|(_, v)| v).collect());
    let expected = call(&IvyJson::object([
        ("operation", "files".into()),
        ("root", IvyJson::String(node_path::from_native(&fixture.0))),
        ("cases", IvyJson::Array(cases)),
    ]));
    let original =
        node_path::node_filesystem_path(&node_path::from_native(&fixture.0.join("original")))
            .unwrap();
    fs::remove_dir_all(original).unwrap();
    fs::remove_dir_all(physical).unwrap();
    pair(case_name, actual, expected);
}
