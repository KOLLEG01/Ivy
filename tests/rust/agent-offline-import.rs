//! Exercise the real native stdin CLIs without Node, ICU libraries or a service owner.
use ivy_agent_manager::{
    environment_text::{BEGIN_MCP, BEGIN_PERMISSIONS, END_MCP, END_PERMISSIONS},
    skills_files::{skills_paths, tree_hash},
};
use ivy_contracts::{
    IvyJson as J, JsString,
    hash::{digest, hash_json},
    json::{parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::node_path::{self, Context};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("ivy-offline-entry-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn package(&self, legacy: bool) -> PathBuf {
        let directory = self.0.join(if legacy {
            "legacy command"
        } else {
            "native command"
        });
        fs::create_dir_all(&directory).unwrap();
        let source = if legacy {
            env!("CARGO_BIN_EXE_ivy-agent-mcp-import")
        } else {
            env!("CARGO_BIN_EXE_ivy-agent-offline-import")
        };
        let path = directory.join(Path::new(source).file_name().unwrap());
        fs::copy(source, &path).unwrap();
        path
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("Retained offline import fixture: {}", self.0.display());
            return;
        }
        let actual = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            actual
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-offline-entry-")
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn field<'a>(value: &'a J, key: &str) -> &'a J {
    value.required(key).unwrap()
}
fn path(value: &Path) -> J {
    J::String(node_path::from_native(value))
}
fn read_json(path: &Path) -> J {
    parse(&fs::read_to_string(path).unwrap(), usize::MAX).unwrap()
}
fn compare(left: &JsString, right: &JsString) -> std::cmp::Ordering {
    ivy_agent_manager::runtime::compare_names(left, right, false)
}
fn invoke(executable: &Path, cwd: &Path, input: &[u8], arguments: &[&str]) -> (i32, J) {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.env(
        "PATH",
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
    );
    #[cfg(unix)]
    command.env("PATH", "");
    command.env("IVY_TEST_LOCALE_RUNTIME", cwd.join("absent-locale"));
    let mut child = command.spawn().unwrap();
    let written = child.stdin.take().unwrap().write_all(input);
    let output = child.wait_with_output().unwrap();
    if output.status.success() {
        written.unwrap();
    }
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-only-credential"));
    let value = parse(
        std::str::from_utf8(&output.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(value.as_object().unwrap().len(), 1);
    (output.status.code().unwrap(), value)
}
fn run(executable: &Path, cwd: &Path, input: &J) -> (i32, J) {
    invoke(
        executable,
        cwd,
        stringify(input, MANAGEMENT_FRAME_BYTES).unwrap().as_bytes(),
        &[],
    )
}
fn code(result: &(i32, J), exit: i32, expected: &str) {
    assert_eq!(result.0, exit, "{:?}", result.1);
    assert_eq!(
        field(field(field(&result.1, "error"), "data"), "code"),
        &J::from(expected)
    );
}
fn authority() -> String {
    hash_json(&J::object([
        ("hive", "https://fixture.example".into()),
        ("hostId", "fixture".into()),
    ]))
    .unwrap()
}
fn skills_snapshot(base: &Path, version: f64, in_place: bool) -> (J, PathBuf, PathBuf) {
    let target = base.join("skills");
    let name = if version == 1.0 {
        "ivy-managed-alpha"
    } else {
        "alpha"
    };
    let directory = target.join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), "retained α").unwrap();
    let hash = tree_hash(&node_path::from_native(&directory), &compare)
        .unwrap()
        .unwrap();
    let paths = skills_paths(
        &node_path::from_native(&target),
        &Context::current().unwrap(),
    )
    .unwrap();
    let source = if in_place {
        node_path::filesystem_path(&paths.state_path)
    } else {
        base.join("legacy.json")
    };
    let state = J::object([
        ("schemaVersion", J::Number(version)),
        ("authority", authority().into()),
        ("applied", J::object([(name, hash.into())])),
        ("appliedVersion", "retained-version".into()),
        ("extra", J::Array(vec!["metadata 😀".into(), J::Null])),
    ]);
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, stringify(&state, usize::MAX).unwrap()).unwrap();
    (
        J::object([
            ("kind", "skills".into()),
            ("sourceState", path(&source)),
            ("root", path(&target)),
            ("hostId", "fixture".into()),
            ("hiveUrl", "https://fixture.example".into()),
        ]),
        source,
        target,
    )
}

#[test]
fn native_skills_command_imports_both_versions_without_a_locale_bundle_and_repeats_safely() {
    let root = Root::new();
    let executable = root.package(false);
    for (version, in_place) in [(1.0, false), (1.0, true), (2.0, false), (2.0, true)] {
        let (request, source, target) = skills_snapshot(
            &root.0.join(format!("v{version}-{in_place}")),
            version,
            in_place,
        );
        let source_bytes = fs::read(&source).unwrap();
        let result = run(&executable, &root.0, &request);
        assert_eq!(result.0, 0, "{:?}", result.1);
        let imported = field(&result.1, "result");
        let current_path =
            node_path::filesystem_path(field(imported, "statePath").as_js_string().unwrap());
        let current = read_json(&current_path);
        assert_eq!(field(&current, "schemaVersion"), &J::Number(2.0));
        assert_eq!(
            field(&current, "extra"),
            &J::Array(vec!["metadata 😀".into(), J::Null])
        );
        assert_eq!(
            field(&current, "appliedVersion"),
            &J::from("retained-version")
        );
        let current_hash = tree_hash(&node_path::from_native(&target.join("alpha")), &compare)
            .unwrap()
            .unwrap();
        assert_eq!(
            field(field(&current, "applied"), "alpha"),
            &J::from(current_hash)
        );
        assert_eq!(
            fs::read(target.join("alpha/SKILL.md")).unwrap(),
            "retained α".as_bytes()
        );
        if !in_place {
            assert_eq!(fs::read(&source).unwrap(), source_bytes);
        }
        if let Some(backup) = imported.get("sourceBackup").and_then(J::as_js_string) {
            assert_eq!(
                fs::read(node_path::filesystem_path(backup)).unwrap(),
                source_bytes
            );
        }
        let repeated = run(&executable, &root.0, &request);
        assert_eq!(repeated.0, 0, "{:?}", repeated.1);
        assert_eq!(
            field(field(&repeated.1, "result"), "unchanged"),
            &J::Bool(true)
        );
        assert_eq!(
            field(field(&repeated.1, "result"), "copiedSkills"),
            &J::Number(0.0)
        );
    }
    assert_eq!(
        fs::read_dir(executable.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn skills_command_refusals_keep_source_local_edits_and_foreign_targets() {
    let root = Root::new();
    let executable = root.package(false);
    for mode in ["local-edit", "collision", "authority"] {
        let (mut request, source, target) = skills_snapshot(&root.0.join(mode), 1.0, false);
        let source_bytes = fs::read(&source).unwrap();
        match mode {
            "local-edit" => {
                fs::write(target.join("ivy-managed-alpha/SKILL.md"), "local edit").unwrap()
            }
            "collision" => {
                fs::create_dir(target.join("alpha")).unwrap();
                fs::write(target.join("alpha/SKILL.md"), "foreign").unwrap();
            }
            _ => {
                request.insert("hostId", "other-host".into());
            }
        }
        code(&run(&executable, &root.0, &request), 1, "target_conflict");
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        if mode == "collision" {
            assert_eq!(fs::read(target.join("alpha/SKILL.md")).unwrap(), b"foreign");
        } else {
            assert!(!target.join("alpha").exists());
        }
    }
}

#[test]
fn unified_and_legacy_mcp_commands_retain_source_metadata_and_the_six_field_interface() {
    let root = Root::new();
    let executable = root.package(false);
    let home = root.0.join("mcp-home");
    fs::create_dir_all(home.join(".ivy-agent-mcp")).unwrap();
    let mcp = format!("{BEGIN_MCP}\n{END_MCP}");
    let permissions = format!(
        "{BEGIN_PERMISSIONS}\napproval_policy = \"never\"\ndefault_permissions = \":danger-full-access\"\n{END_PERMISSIONS}"
    );
    let config = format!("# user\ncustom = true\n{mcp}\n{permissions}\n");
    let state = J::object([
        ("schemaVersion", J::Number(1.0)),
        ("authority", authority().into()),
        ("appliedBlockHash", digest(&mcp).into()),
        ("appliedPermissionsBlockHash", digest(&permissions).into()),
        ("appliedVersion", "prior".into()),
        ("extra", "retained metadata".into()),
    ]);
    let state_bytes = stringify(&state, usize::MAX).unwrap();
    fs::write(home.join("config.toml"), &config).unwrap();
    fs::write(home.join(".ivy-agent-mcp/state.json"), &state_bytes).unwrap();
    let mut request = J::object([
        ("kind", "mcp".into()),
        ("home", path(&home)),
        ("hostId", "fixture".into()),
        ("hiveUrl", "https://fixture.example".into()),
        ("credential", "fixture-only-credential".into()),
        (
            "document",
            parse(
                r#"{"schemaVersion":1,"hostId":null,"enabled":true,"servers":[]}"#,
                usize::MAX,
            )
            .unwrap(),
        ),
        (
            "openai",
            parse(
                r#"{"revision":0,"configuration":{"schemaVersion":1,"appId":null}}"#,
                usize::MAX,
            )
            .unwrap(),
        ),
    ]);
    let result = run(&executable, &root.0, &request);
    assert_eq!(result.0, 0, "{:?}", result.1);
    let imported = field(&result.1, "result");
    assert_eq!(field(imported, "changed"), &J::Bool(true));
    let backup =
        node_path::filesystem_path(field(imported, "sourceBackup").as_js_string().unwrap());
    assert_eq!(
        fs::read(backup.join("config.toml")).unwrap(),
        config.as_bytes()
    );
    assert_eq!(
        fs::read(backup.join("state.json")).unwrap(),
        state_bytes.as_bytes()
    );
    let current = read_json(&home.join(".ivy-agent-mcp/state.json"));
    assert_eq!(field(&current, "extra"), &J::from("retained metadata"));
    assert_eq!(field(&current, "schemaVersion"), &J::Number(2.0));
    assert!(
        fs::read_to_string(home.join("config.toml"))
            .unwrap()
            .contains("custom = true")
    );
    request.remove("kind");
    let legacy = run(&root.package(true), &root.0, &request);
    assert_eq!(legacy.0, 0, "{:?}", legacy.1);
    assert_eq!(
        field(field(&legacy.1, "result"), "changed"),
        &J::Bool(false)
    );
}

#[test]
fn strict_stdin_and_argument_preflight_fail_before_filesystem_changes() {
    let root = Root::new();
    let executable = root.package(false);
    for bytes in [
        b"{}".as_slice(),
        b"[]",
        b"null",
        b"{",
        b"\xff",
        br#"{"kind":"other"}"#,
    ] {
        code(
            &invoke(&executable, &root.0, bytes, &[]),
            2,
            "invalid_arguments",
        );
    }
    code(
        &invoke(&executable, &root.0, b"{}", &["unexpected"]),
        2,
        "invalid_arguments",
    );
    code(
        &invoke(
            &executable,
            &root.0,
            &vec![b' '; MANAGEMENT_FRAME_BYTES + 1],
            &[],
        ),
        1,
        "limit_exceeded",
    );
    let (mut request, source, target) = skills_snapshot(&root.0.join("strict"), 1.0, false);
    let before = fs::read(&source).unwrap();
    request.insert("extra", true.into());
    code(&run(&executable, &root.0, &request), 2, "invalid_arguments");
    request.remove("extra");
    request.insert("root", "relative".into());
    code(&run(&executable, &root.0, &request), 2, "invalid_arguments");
    assert_eq!(fs::read(source).unwrap(), before);
    assert!(!target.join("alpha").exists());
}
