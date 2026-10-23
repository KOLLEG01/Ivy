use ivy_agent_manager::skills_files::skills_paths;
use ivy_contracts::{
    IvyJson, JsString,
    hash::digest,
    json::{canonical, parse, stringify},
    limits::MANAGEMENT_FRAME_BYTES,
};
use ivy_host_runtime::node_path::{self, Context};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let parent = std::env::temp_dir();
        assert!(parent.is_absolute());
        let path = parent.join(format!("ivy-offline-entry-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let actual = fs::canonicalize(&self.0).unwrap();
        assert_eq!(
            actual.parent(),
            Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
        );
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("ivy-offline-entry-")
        );
        assert!(
            !fs::symlink_metadata(&self.0)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(actual).unwrap();
    }
}
fn field<'a>(v: &'a IvyJson, key: &str) -> &'a IvyJson {
    v.get(key).unwrap()
}
fn js<'a>(v: &'a IvyJson, key: &str) -> &'a JsString {
    field(v, key).as_js_string().unwrap()
}
fn path(v: &IvyJson, key: &str) -> PathBuf {
    node_path::filesystem_path(js(v, key))
}
fn value(path: &Path) -> IvyJson {
    IvyJson::String(node_path::from_native(path))
}
fn same(actual: &IvyJson, expected: &IvyJson) {
    assert_eq!(
        canonical(actual, usize::MAX).unwrap(),
        canonical(expected, usize::MAX).unwrap()
    );
}
fn proof(label: &str, observations: IvyJson) {
    if let Some(path) = std::env::var_os("IVY_UNIFIED_IMPORT_PROOF") {
        let row = IvyJson::object([("label", label.into()), ("observations", observations)]);
        writeln!(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap(),
            "{}",
            canonical(&row, usize::MAX).unwrap()
        )
        .unwrap();
    }
}
fn fixture(input: IvyJson) -> IvyJson {
    let mut child = Command::new("node")
        .arg("tests/fixtures/agent-skills-files-oracle.mjs")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            stringify(&input, MANAGEMENT_FRAME_BYTES)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    parse(
        std::str::from_utf8(&out.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap()
}
fn prepare(base: &Path, version: u32, in_place: bool) -> IvyJson {
    fixture(IvyJson::object([
        ("action", "migrationPrepare".into()),
        ("root", value(base)),
        ("version", IvyJson::Number(version as f64)),
        ("inPlace", in_place.into()),
    ]))
}
fn request(prepared: &IvyJson) -> IvyJson {
    IvyJson::object([
        ("kind", "skills".into()),
        ("sourceState", field(prepared, "source").clone()),
        ("root", field(prepared, "target").clone()),
        ("hostId", "fixture".into()),
        ("hiveUrl", "https://fixture.example".into()),
    ])
}
fn packaged(root: &Path, runtime: bool, legacy: bool) -> PathBuf {
    let directory = root.join(format!("program-{runtime}-{legacy}"));
    fs::create_dir(&directory).unwrap();
    let source = if legacy {
        env!("CARGO_BIN_EXE_ivy-agent-mcp-import")
    } else {
        env!("CARGO_BIN_EXE_ivy-agent-offline-import")
    };
    let executable = directory.join(Path::new(source).file_name().unwrap());
    fs::copy(source, &executable).unwrap();
    if runtime {
        let from = PathBuf::from(
            std::env::var_os("IVY_TEST_LOCALE_RUNTIME")
                .expect("Provide the qualified packaged locale runtime"),
        );
        #[cfg(windows)]
        let names = ["icudt78.dll", "icuuc78.dll", "icuin78.dll", "LICENSE.icu"];
        #[cfg(not(windows))]
        let names = [
            "libicudata.so.78",
            "libicuuc.so.78",
            "libicui18n.so.78",
            "LICENSE.icu",
        ];
        for name in names {
            fs::copy(from.join(name), directory.join(name)).unwrap();
        }
    }
    executable
}
fn invoke(executable: &Path, cwd: &Path, bytes: &[u8], args: &[&str]) -> (i32, IvyJson) {
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.env(
        "PATH",
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32"),
    );
    let mut child = command.spawn().unwrap();
    let written = child.stdin.take().unwrap().write_all(bytes);
    let out = child.wait_with_output().unwrap();
    if out.status.success() {
        written.unwrap();
    }
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("fixture-only"),
        "stdin credential entered output"
    );
    let result = parse(
        std::str::from_utf8(&out.stdout).unwrap(),
        MANAGEMENT_FRAME_BYTES,
    )
    .unwrap();
    assert_eq!(result.as_object().unwrap().len(), 1);
    (out.status.code().unwrap(), result)
}
fn run(executable: &Path, cwd: &Path, request: &IvyJson) -> (i32, IvyJson) {
    invoke(
        executable,
        cwd,
        stringify(request, MANAGEMENT_FRAME_BYTES)
            .unwrap()
            .as_bytes(),
        &[],
    )
}
fn assert_code(result: &(i32, IvyJson), exit: i32, expected: &str) {
    assert_eq!(result.0, exit, "{:?}", result.1);
    assert_eq!(
        field(field(field(&result.1, "error"), "data"), "code"),
        &IvyJson::from(expected)
    );
}
fn original_tree(target: &Path) -> String {
    field(
        &fixture(IvyJson::object([
            ("action", "treeExisting".into()),
            ("target", value(target)),
        ])),
        "result",
    )
    .as_str()
    .unwrap()
}

#[test]
fn packaged_command_imports_original_skills_and_retains_sources_with_current_format_repeat() {
    let root = Root::new();
    let executable = packaged(&root.0, true, false);
    for (version, in_place) in [(1, false), (1, true), (2, false), (2, true)] {
        let base = root.0.join(format!("v{version}-{in_place}"));
        let prepared = prepare(&base, version, in_place);
        let source = path(&prepared, "source");
        let mut state = field(&prepared, "state").clone();
        if version == 1 && !in_place {
            let tree = base.join("skills/ivy-managed-alpha");
            for name in [
                "é.txt",
                "e\u{301}.txt",
                "Å.txt",
                "A\u{30a}.txt",
                "a.txt",
                "z.txt",
            ] {
                fs::write(tree.join(name), name).unwrap();
            }
            let mut applied = field(&state, "applied").clone();
            applied.insert("ivy-managed-alpha", original_tree(&tree).into());
            state.insert("applied", applied);
            fs::write(
                &source,
                stringify(&state, MANAGEMENT_FRAME_BYTES).unwrap() + "\n",
            )
            .unwrap();
        }
        let source_bytes = fs::read(&source).unwrap();
        let input = request(&prepared);
        let (code, result) = run(&executable, &root.0, &input);
        assert_eq!(code, 0, "{result:?}");
        let output = field(&result, "result");
        assert_eq!(
            field(output, "copiedSkills"),
            &IvyJson::Number(if version == 1 { 2.0 } else { 0.0 })
        );
        assert_eq!(
            field(output, "unchanged"),
            &IvyJson::Bool(version == 2 && in_place)
        );
        assert_eq!(path(output, "statePath"), path(&prepared, "statePath"));
        if !in_place || version == 2 {
            assert_eq!(fs::read(&source).unwrap(), source_bytes);
        }
        if !field(output, "sourceBackup").is_null() {
            assert_eq!(
                fs::read(path(output, "sourceBackup")).unwrap(),
                source_bytes
            );
        }
        let current = parse(
            &fs::read_to_string(path(output, "statePath")).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap();
        assert_eq!(field(&current, "schemaVersion"), &IvyJson::Number(2.0));
        for name in ["authority", "appliedVersion", "extra"] {
            same(field(&current, name), field(&state, name));
        }
        for name in ["alpha", "beta"] {
            let old = if version == 1 {
                format!("ivy-managed-{name}")
            } else {
                name.into()
            };
            assert_eq!(
                fs::read_to_string(base.join("skills").join(&old).join("SKILL.md")).unwrap(),
                format!("retained {name}")
            );
            assert_eq!(
                original_tree(&base.join("skills").join(&old)),
                field(field(&state, "applied"), &old).as_str().unwrap()
            );
            assert_eq!(
                original_tree(&base.join("skills").join(name)),
                field(field(&current, "applied"), name).as_str().unwrap()
            );
        }
        let repeated = run(&executable, &root.0, &input);
        assert_eq!(repeated.0, 0);
        let repeated = field(&repeated.1, "result");
        assert_eq!(field(repeated, "unchanged"), &IvyJson::Bool(true));
        assert_eq!(field(repeated, "copiedSkills"), &IvyJson::Number(0.0));
        assert!(field(repeated, "sourceBackup").is_null());
        proof(
            &format!("skills-v{version}-{in_place}"),
            IvyJson::object([
                ("sourceDigest", digest(&source_bytes).into()),
                ("first", result),
                ("repeat", repeated.clone()),
                ("current", current),
            ]),
        );
    }
}

#[test]
fn command_refusals_retain_source_local_edits_and_interrupted_import_evidence() {
    let root = Root::new();
    let executable = packaged(&root.0, true, false);
    for mode in [
        "local-edit",
        "missing-tree",
        "target-collision",
        "wrong-authority",
        "existing-owner",
        "interrupted",
    ] {
        let base = root.0.join(mode);
        let prepared = prepare(&base, 1, false);
        let source = path(&prepared, "source");
        let source_bytes = fs::read(&source).unwrap();
        let mut input = request(&prepared);
        match mode {
            "local-edit" => {
                fs::write(base.join("skills/ivy-managed-alpha/SKILL.md"), "local edit").unwrap()
            }
            "missing-tree" => {
                fs::remove_file(base.join("skills/ivy-managed-beta/SKILL.md")).unwrap()
            }
            "target-collision" | "interrupted" => {
                fs::create_dir(base.join("skills/alpha")).unwrap();
                fs::write(base.join("skills/alpha/SKILL.md"), "unowned partial copy").unwrap();
            }
            "wrong-authority" => input.insert("hostId", "other-host".into()),
            "existing-owner" => {
                let state = path(&prepared, "statePath");
                fs::create_dir(state.parent().unwrap()).unwrap();
                fs::write(state, "{}").unwrap();
            }
            _ => unreachable!(),
        }
        let paths = skills_paths(js(&prepared, "target"), &Context::current().unwrap()).unwrap();
        let retained = node_path::filesystem_path(&paths.state_root).join(".import-retained");
        if mode == "interrupted" {
            fs::create_dir_all(&retained).unwrap();
            fs::write(
                retained.join("inspection.txt"),
                "inspect interrupted import",
            )
            .unwrap();
        }
        let before_alpha = fs::read(base.join("skills/ivy-managed-alpha/SKILL.md")).unwrap();
        let result = run(&executable, &root.0, &input);
        assert_code(&result, 1, "target_conflict");
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(
            fs::read(base.join("skills/ivy-managed-alpha/SKILL.md")).unwrap(),
            before_alpha
        );
        if mode == "target-collision" || mode == "interrupted" {
            assert_eq!(
                fs::read_to_string(base.join("skills/alpha/SKILL.md")).unwrap(),
                "unowned partial copy"
            );
        } else {
            assert!(!base.join("skills/alpha").exists());
        }
        if mode == "interrupted" {
            assert_eq!(
                fs::read_to_string(retained.join("inspection.txt")).unwrap(),
                "inspect interrupted import"
            );
        }
        proof(
            mode,
            IvyJson::object([
                ("sourceDigest", digest(&source_bytes).into()),
                ("response", result.1),
            ]),
        );
    }
}

#[test]
fn strict_stdin_preflight_and_byte_bound_precede_locale_loading_or_file_changes() {
    let root = Root::new();
    let executable = packaged(&root.0, false, false);
    let input = IvyJson::object([
        ("kind", "skills".into()),
        ("sourceState", value(&root.0.join("absent-state"))),
        ("root", value(&root.0.join("absent-root"))),
        ("hostId", "fixture".into()),
        ("hiveUrl", "https://fixture.example".into()),
    ]);
    for mode in [
        "kind",
        "missing",
        "mixed",
        "relative-source",
        "relative-root",
        "null-host",
        "long-host",
        "credential",
    ] {
        let mut bad = input.clone();
        match mode {
            "kind" => bad.insert("kind", "unknown".into()),
            "missing" => {
                bad = IvyJson::object([("kind", "skills".into())]);
            }
            "mixed" => bad.insert("document", IvyJson::Null),
            "relative-source" => bad.insert("sourceState", "relative".into()),
            "relative-root" => bad.insert("root", "relative".into()),
            "null-host" => bad.insert("hostId", IvyJson::Null),
            "long-host" => bad.insert("hostId", "😀".repeat(129).into()),
            "credential" => bad.insert("credential", "fixture-only".into()),
            _ => unreachable!(),
        }
        let result = run(&executable, &root.0, &bad);
        assert_code(&result, 2, "invalid_arguments");
        proof(mode, result.1);
    }
    for (name, bytes, args, exit, code) in [
        ("bad-json", b"{".to_vec(), vec![], 2, "invalid_arguments"),
        ("non-utf8", vec![0xff], vec![], 2, "invalid_arguments"),
        (
            "extra-json",
            b"{}\n{}".to_vec(),
            vec![],
            2,
            "invalid_arguments",
        ),
        (
            "arguments",
            b"{}".to_vec(),
            vec!["--unexpected"],
            2,
            "invalid_arguments",
        ),
        (
            "over-byte-limit",
            vec![b' '; MANAGEMENT_FRAME_BYTES + 1],
            vec![],
            1,
            "limit_exceeded",
        ),
    ] {
        let result = invoke(&executable, &root.0, &bytes, &args);
        assert_code(&result, exit, code);
        proof(
            name,
            IvyJson::object([
                ("inputBytes", IvyJson::Number(bytes.len() as f64)),
                ("response", result.1),
            ]),
        );
    }
    assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    assert!(!root.0.join("absent-root").exists());
}

#[test]
fn skills_requires_runtime_beside_executable_and_rejects_ambient_or_partial_libraries() {
    let root = Root::new();
    let bare = packaged(&root.0, false, false);
    let complete = packaged(&root.0, true, false);
    let prepared = prepare(&root.0.join("selected"), 1, false);
    let source = path(&prepared, "source");
    let before = fs::read(&source).unwrap();
    let input = request(&prepared);
    for (mode, cwd) in [
        ("missing", root.0.as_path()),
        ("ambient-cwd", complete.parent().unwrap()),
    ] {
        let result = run(&bare, cwd, &input);
        assert_code(&result, 1, "locale_unavailable");
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(!path(&prepared, "statePath").exists());
        assert!(!root.0.join("selected/skills/alpha").exists());
        proof(mode, result.1);
    }
    #[cfg(windows)]
    let data = "icudt78.dll";
    #[cfg(not(windows))]
    let data = "libicudata.so.78";
    fs::copy(
        complete.parent().unwrap().join(data),
        bare.parent().unwrap().join(data),
    )
    .unwrap();
    let result = run(&bare, &root.0, &input);
    assert_code(&result, 1, "locale_unavailable");
    assert_eq!(fs::read(source).unwrap(), before);
    assert!(!path(&prepared, "statePath").exists());
    proof("partial-runtime", result.1);
}

fn mcp_row() -> &'static IvyJson {
    static ROW: OnceLock<IvyJson> = OnceLock::new();
    ROW.get_or_init(|| {
        let out = Command::new("node")
            .arg("tests/fixtures/agent-mcp-output-oracle.mjs")
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let all = parse(
            std::str::from_utf8(&out.stdout).unwrap(),
            MANAGEMENT_FRAME_BYTES,
        )
        .unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .find(|row| {
                row.get("state").and_then(|s| s.get("schemaVersion")) == Some(&IvyJson::Number(1.0))
                    && field(row, "expected").get("result").is_some()
                    && field(row, "source")
                        .as_str()
                        .unwrap()
                        .contains(ivy_agent_manager::environment_text::BEGIN_MCP)
            })
            .unwrap()
            .clone()
    })
}
#[test]
fn unified_mcp_and_legacy_command_keep_snapshot_contract_without_skills_runtime() {
    let root = Root::new();
    let row = mcp_row();
    for legacy in [false, true] {
        let executable = packaged(&root.0, false, legacy);
        let home = root.0.join(format!("mcp-{legacy}"));
        fs::create_dir(&home).unwrap();
        let source = field(row, "fileSource").as_str().unwrap();
        fs::write(home.join("config.toml"), &source).unwrap();
        let state = stringify(field(row, "state"), MANAGEMENT_FRAME_BYTES).unwrap() + "\n";
        fs::create_dir(home.join(".ivy-agent-mcp")).unwrap();
        fs::write(home.join(".ivy-agent-mcp/state.json"), &state).unwrap();
        let mut input = IvyJson::object([
            ("home", value(&home)),
            ("hostId", "fixture".into()),
            ("hiveUrl", field(row, "hive").clone()),
            ("credential", field(row, "credential").clone()),
            ("document", field(row, "document").clone()),
            (
                "openai",
                IvyJson::object([
                    ("revision", IvyJson::Number(0.0)),
                    (
                        "configuration",
                        IvyJson::object([
                            ("schemaVersion", IvyJson::Number(1.0)),
                            ("appId", field(row, "appId").clone()),
                        ]),
                    ),
                ]),
            ),
        ]);
        if !legacy {
            input.insert("kind", "mcp".into());
        }
        let result = run(&executable, &root.0, &input);
        assert_eq!(result.0, 0, "{:?}", result.1);
        let output = field(&result.1, "result");
        assert_eq!(field(output, "changed"), &IvyJson::Bool(true));
        let backup = path(output, "sourceBackup");
        assert_eq!(
            fs::read_to_string(backup.join("config.toml")).unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(backup.join("state.json")).unwrap(),
            state
        );
        assert_eq!(
            fs::read_to_string(path(output, "target")).unwrap(),
            field(field(field(row, "expected"), "result"), "text")
                .as_str()
                .unwrap()
        );
        let repeat = run(&executable, &root.0, &input);
        assert_eq!(repeat.0, 0);
        assert_eq!(
            field(field(&repeat.1, "result"), "changed"),
            &IvyJson::Bool(false)
        );
        let mut wrong = input;
        wrong.insert(
            "kind",
            if legacy {
                "mcp".into()
            } else {
                "skills".into()
            },
        );
        assert_code(&run(&executable, &root.0, &wrong), 2, "invalid_arguments");
        proof(
            &format!("mcp-legacy-{legacy}"),
            IvyJson::object([
                ("first", result.1),
                ("repeat", repeat.1),
                ("sourceDigest", digest(source).into()),
            ]),
        );
    }
}
