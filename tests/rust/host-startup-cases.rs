use ivy_contracts::{IvyJson as J, IvyResult, JsString, json};
use ivy_host_runtime::{
    host_config::{self, HostContext},
    native_account,
    node_path::{self, Context, Style},
    paths, runtime_maintenance,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ivy-startup-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn jp(p: &Path) -> J {
    J::String(node_path::from_native(p))
}
fn val(p: &Path, s: &str) -> J {
    jp(&p.join(s))
}
fn result(value: IvyResult<J>) -> J {
    match value {
        Ok(v) => J::object([("ok", true.into()), ("value", v)]),
        Err(e) => J::object([("ok", false.into()), ("error", e.to_wire())]),
    }
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
fn replace(value: &J, from: &JsString, to: &JsString) -> J {
    match value {
        J::String(s) => {
            let mut output = Vec::new();
            let mut i = 0;
            while i < s.0.len() {
                if s.0[i..].starts_with(&from.0) {
                    output.extend_from_slice(&to.0);
                    i += from.0.len();
                } else {
                    output.push(s.0[i]);
                    i += 1;
                }
            }
            J::String(JsString(output))
        }
        J::Array(a) => J::Array(a.iter().map(|v| replace(v, from, to)).collect()),
        J::Object(o) => J::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), replace(v, from, to)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn oracle(input: &J, home: &Path) -> J {
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--disable-warning=ExperimentalWarning",
            "tests/fixtures/host-startup-oracle.mjs",
        ])
        .current_dir(repo())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("CODEX_HOME")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json::stringify(input, usize::MAX).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    json::parse(std::str::from_utf8(&output.stdout).unwrap(), usize::MAX).unwrap()
}
fn context(home: &Path) -> HostContext {
    let home = node_path::from_native(home);
    HostContext {
        paths: Context::current().unwrap(),
        environment: BTreeMap::from([
            ("HOME".into(), home.clone()),
            ("USERPROFILE".into(), home.clone()),
        ]),
        account_home: home,
    }
}
fn setup(actions: &J) {
    for a in actions.as_array().unwrap() {
        let p = node_path::to_native(a.required("path").unwrap().as_js_string().unwrap());
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        match a.required("kind").unwrap().as_str().unwrap().as_str() {
            "directory" => fs::create_dir_all(p).unwrap(),
            "file" => fs::write(
                p,
                a.get("json")
                    .map(|v| json::stringify(v, usize::MAX).unwrap())
                    .or_else(|| a.get("content").map(|v| v.as_str().unwrap()))
                    .unwrap_or_else(|| "fixture".into()),
            )
            .unwrap(),
            "link" => {
                let target =
                    node_path::to_native(a.required("target").unwrap().as_js_string().unwrap());
                #[cfg(windows)]
                {
                    let output = Command::new("cmd.exe")
                        .args(["/d", "/c", "mklink", "/J"])
                        .arg(&p)
                        .arg(target)
                        .output()
                        .unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, p).unwrap();
            }
            _ => panic!("Unknown setup"),
        }
    }
}
fn tree(root: &Path) -> J {
    fn walk(root: &Path, p: &Path, values: &mut Vec<J>) {
        let mut entries: Vec<_> = fs::read_dir(p).unwrap().map(|v| v.unwrap()).collect();
        entries.sort_by_key(|v| node_path::from_native(Path::new(&v.file_name())));
        for entry in entries {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let kind = if meta.is_symlink() {
                "link"
            } else if meta.is_dir() {
                "directory"
            } else {
                "file"
            };
            let mut relative = node_path::from_native(path.strip_prefix(root).unwrap());
            if cfg!(windows) {
                for c in &mut relative.0 {
                    if *c == 92 {
                        *c = 47;
                    }
                }
            }
            values.push(J::object([
                ("path", J::String(relative)),
                ("kind", kind.into()),
            ]));
            if meta.is_dir() && !meta.is_symlink() {
                walk(root, &path, values);
            }
        }
    }
    let mut values = Vec::new();
    if root.exists() {
        walk(root, root, &mut values);
    }
    J::Array(values)
}
fn action(kind: &str, path: J) -> J {
    J::object([("kind", kind.into()), ("path", path)])
}
fn file(path: J, content: &str) -> J {
    let mut a = action("file", path);
    a.insert("content", content.into());
    a
}
fn link(path: J, target: J) -> J {
    let mut a = action("link", path);
    a.insert("target", target);
    a
}
fn base(root: &Path) -> J {
    J::object([
        ("schemaVersion", J::Number(1.0)),
        ("hostId", "fixture-host".into()),
        ("runtimeRoot", val(root, "runtime")),
        ("artifactRoot", val(root, "artifacts")),
        ("stagingRoot", val(root, "staging")),
        ("publicBaseUrl", "https://example.invalid/ivy".into()),
        (
            "executables",
            J::object([("node", val(root, "tools/node"))]),
        ),
        (
            "instances",
            J::Array(vec![instance(
                "fixture",
                "fixture",
                J::object([] as [(&str, J); 0]),
            )]),
        ),
    ])
}
fn instance(id: &str, component: &str, settings: J) -> J {
    J::object([
        ("instanceId", id.into()),
        ("serviceNodeId", format!("node-{id}").into()),
        ("componentId", component.into()),
        ("enabled", true.into()),
        ("engine", "process".into()),
        ("settings", settings),
    ])
}
fn service_paths(root: &Path) -> J {
    J::object([
        ("data", val(root, "data")),
        ("work", val(root, "work")),
        ("logs", val(root, "logs")),
    ])
}
fn agent(root: &Path) -> J {
    instance(
        "agent-renamed",
        "agent-manager",
        J::object([
            ("codexHome", val(root, "user-codex")),
            ("internalProjectRoot", val(root, "internal")),
            ("projectRoot", val(root, "projects")),
            ("appServer", J::object([("mode", "external-proxy".into())])),
        ]),
    )
}

#[tokio::test]
async fn host_configuration_matches_original_admission_and_directory_effects() {
    let root = Root::new();
    let rust = root.0.join("rust");
    let node = root.0.join("node");
    let account = root.0.join("account");
    let names = [
        "legacy",
        "modern",
        "no-instances",
        "defaults",
        "duplicate-id",
        "duplicate-node",
        "root-overlap",
        "artifact-config",
        "config-mismatch",
        "service-collision",
        "service-relative",
        "service-in-artifacts",
        "service-over-config",
        "modern-service-in-state",
        "legacy-service-in-state",
        "relative-executable",
        "invalid-id",
        "invalid-component",
        "docker",
        "schema",
        "root-file",
        "service-file",
        "repeated-explicit",
        "paths-order",
        "agent-default",
        "agent-owned",
        "internal-relative",
        "internal-in-state",
        "internal-over-config",
        "project-relative",
        "project-in-state",
        "project-ancestor",
        "shared-in-state",
        "shared-in-service",
        "shared-in-projects",
        "shared-ancestor",
        "shared-null",
        "shared-number",
        "unknown-mode",
        "claude",
        "config-alias",
        "roots-alias",
        "service-alias",
        "internal-alias",
        "shared-alias",
        "normal-alias",
        "invalid-url",
        "credential-url",
        "remote-http",
        "loopback-http",
        "utf16-url",
        "read-file",
        "missing-file",
        "directory-file",
        "invalid-json",
        "oversize-file",
    ];
    let mut cases = Vec::new();
    for name in names {
        let r = rust.join(name);
        let mut c = base(&r);
        let mut actions = vec![action("directory", jp(&r))];
        let mut config_path = r.join("host.json");
        let mut operation = "checked";
        let mut instances = c
            .required("instances")
            .unwrap()
            .as_array()
            .unwrap()
            .to_vec();
        if name.starts_with("agent-")
            || [
                "internal-relative",
                "internal-in-state",
                "internal-over-config",
                "project-relative",
                "project-in-state",
                "project-ancestor",
                "shared-in-state",
                "shared-in-service",
                "shared-in-projects",
                "shared-ancestor",
                "shared-null",
                "shared-number",
                "unknown-mode",
                "claude",
                "internal-alias",
                "shared-alias",
                "normal-alias",
            ]
            .contains(&name)
        {
            instances = vec![agent(&r)];
        }
        match name {
            "modern" => c.insert("ivyRoot", val(&r, "ivy")),
            "no-instances" => instances.clear(),
            "defaults" => {
                c.remove("runtimeRoot");
                c.remove("artifactRoot");
                c.remove("stagingRoot");
                c.insert("ivyRoot", val(&r, "ivy"));
            }
            "duplicate-id" => {
                let mut v = instances[0].clone();
                v.insert("serviceNodeId", "other".into());
                instances.push(v);
            }
            "duplicate-node" => {
                let mut v = instances[0].clone();
                v.insert("instanceId", "other".into());
                instances.push(v);
            }
            "root-overlap" => c.insert("stagingRoot", val(&r, "artifacts/child")),
            "artifact-config" => config_path = r.join("artifacts/host.json"),
            "config-mismatch" => c.insert("configPath", val(&r, "other.json")),
            "service-collision" => {
                c.insert("servicesRoot", val(&r, "services"));
                instances.push(instance(
                    "second",
                    "fixture",
                    J::object([] as [(&str, J); 0]),
                ));
            }
            "service-relative" => {
                let mut p = service_paths(&r);
                p.insert("data", "relative".into());
                instances[0].insert("paths", p);
            }
            "service-in-artifacts" => {
                instances[0].insert("paths", service_paths(&r.join("artifacts")))
            }
            "service-over-config" => {
                let mut p = service_paths(&r);
                p.insert("data", jp(&r));
                instances[0].insert("paths", p);
            }
            "modern-service-in-state" => {
                c.insert("servicesRoot", val(&r, "runtime/services"));
            }
            "legacy-service-in-state" => {
                instances[0].insert("paths", service_paths(&r.join("runtime/custom")))
            }
            "relative-executable" => c.insert("executables", J::object([("node", "node".into())])),
            "invalid-id" => instances[0].insert("instanceId", "unsafe.name".into()),
            "invalid-component" => instances[0].insert("componentId", "UpperCase".into()),
            "docker" => {
                instances[0].insert("engine", "docker".into());
                instances[0].insert(
                    "docker",
                    J::object([
                        ("imageRepository", "fixture".into()),
                        ("ports", J::Array(vec![])),
                    ]),
                );
            }
            "schema" => instances[0].remove("enabled").map(|_| ()).unwrap(),
            "root-file" => actions.push(file(val(&r, "artifacts"), "file")),
            "service-file" => actions.push(file(val(&r, "runtime/instances/fixture/data"), "file")),
            "repeated-explicit" => {
                c.insert("servicesRoot", val(&r, "services"));
                let mut v = instance("second", "fixture", J::object([] as [(&str, J); 0]));
                v.insert("paths", service_paths(&r.join("other")));
                instances.push(v);
            }
            "paths-order" => {
                instances[0].insert(
                    "paths",
                    J::object([
                        ("logs", val(&r, "logs")),
                        ("work", val(&r, "blocked/work")),
                        ("data", val(&r, "data")),
                    ]),
                );
                actions.push(file(val(&r, "blocked/work"), "file"));
            }
            "agent-default" => instances[0].insert("settings", J::object([] as [(&str, J); 0])),
            "agent-owned" => {
                let mut s = instances[0].required("settings").unwrap().clone();
                s.insert(
                    "codexHome",
                    val(&r, "runtime/instances/agent-renamed/data/codex-home"),
                );
                s.insert("appServer", J::object([("mode", "owned-stdio".into())]));
                instances[0].insert("settings", s);
            }
            "config-alias" => {
                actions.push(action("directory", val(&r, "configuration")));
                actions.push(link(val(&r, "alias"), val(&r, "configuration")));
                config_path = r.join("alias/host.json");
                c.insert("configPath", val(&r, "configuration/host.json"));
            }
            "roots-alias" => {
                actions.push(action("directory", val(&r, "runtime")));
                actions.push(link(val(&r, "artifacts"), val(&r, "runtime")));
            }
            "service-alias" => {
                actions.push(action("directory", val(&r, "artifacts")));
                actions.push(link(val(&r, "linked"), val(&r, "artifacts")));
                instances[0].insert("paths", service_paths(&r.join("linked")));
            }
            "invalid-url" => c.insert("publicBaseUrl", "not-a-url".into()),
            "credential-url" => c.insert(
                "publicBaseUrl",
                "https://user:pass@example.invalid/ivy".into(),
            ),
            "remote-http" => c.insert("publicBaseUrl", "http://example.invalid/ivy".into()),
            "loopback-http" => c.insert("publicBaseUrl", "http://[::1]:1234/ivy?".into()),
            "utf16-url" => c.insert(
                "publicBaseUrl",
                J::String(JsString(
                    "https://example.invalid/"
                        .encode_utf16()
                        .chain([0xd800])
                        .collect(),
                )),
            ),
            "read-file" | "missing-file" | "directory-file" | "invalid-json" | "oversize-file" => {
                operation = "file";
            }
            _ => {
                let mut s = instances[0].required("settings").unwrap().clone();
                match name {
                    "internal-relative" => s.insert("internalProjectRoot", "relative".into()),
                    "internal-in-state" => {
                        s.insert("internalProjectRoot", val(&r, "runtime/internal"))
                    }
                    "internal-over-config" => s.insert("internalProjectRoot", jp(&r)),
                    "project-relative" => s.insert("projectRoot", "relative".into()),
                    "project-in-state" => s.insert("projectRoot", val(&r, "runtime/projects")),
                    "project-ancestor" => s.insert("projectRoot", jp(&r)),
                    "shared-in-state" => s.insert("codexHome", val(&r, "runtime/codex")),
                    "shared-in-service" => s.insert(
                        "codexHome",
                        val(&r, "runtime/instances/agent-renamed/data/codex"),
                    ),
                    "shared-in-projects" => s.insert("codexHome", val(&r, "internal/codex")),
                    "shared-ancestor" => s.insert("codexHome", jp(&r)),
                    "shared-null" | "shared-number" => s.insert(
                        "appServer",
                        J::object([
                            ("mode", "external-proxy".into()),
                            (
                                "expectedCodexHome",
                                if name == "shared-null" {
                                    J::Null
                                } else {
                                    J::Number(7.0)
                                },
                            ),
                        ]),
                    ),
                    "unknown-mode" => {
                        s.insert("appServer", J::object([("mode", "future-mode".into())]))
                    }
                    "claude" => {
                        s.insert("appServer", J::object([("mode", "claude-adapter".into())]));
                        s.insert(
                            "codexHome",
                            val(&r, "runtime/instances/agent-renamed/data/codex-home"),
                        );
                    }
                    "internal-alias" | "shared-alias" | "normal-alias" => {
                        actions.push(action("directory", val(&r, "artifacts")));
                        actions.push(link(val(&r, "linked"), val(&r, "artifacts")));
                        s.insert(
                            if name == "internal-alias" {
                                "internalProjectRoot"
                            } else if name == "shared-alias" {
                                "codexHome"
                            } else {
                                "projectRoot"
                            },
                            val(&r, "linked/missing"),
                        );
                    }
                    _ => (),
                }
                instances[0].insert("settings", s);
            }
        }
        c.insert("instances", J::Array(instances));
        if operation == "file" {
            match name {
                "read-file" => {
                    let mut a = action("file", jp(&config_path));
                    a.insert("json", c.clone());
                    actions.push(a);
                }
                "directory-file" => actions.push(action("directory", jp(&config_path))),
                "invalid-json" => actions.push(file(jp(&config_path), "{")),
                "oversize-file" => {
                    actions.push(file(jp(&config_path), &" ".repeat(1024 * 1024 + 1)))
                }
                _ => (),
            }
        }
        cases.push(J::object([
            ("id", name.into()),
            ("root", jp(&r)),
            ("config", c),
            ("configurationPath", jp(&config_path)),
            ("operation", operation.into()),
            ("setup", J::Array(actions)),
        ]));
    }
    let request = J::object([
        ("kind", "configuration".into()),
        ("cases", J::Array(cases.clone())),
    ]);
    let expected = oracle(
        &replace(
            &request,
            &node_path::from_native(&rust),
            &node_path::from_native(&node),
        ),
        &account,
    );
    let expected = replace(
        &expected,
        &node_path::from_native(&node),
        &node_path::from_native(&rust),
    );
    let mut records = Vec::new();
    let mut mismatches = Vec::new();
    for (c, e) in cases.iter().zip(expected.as_array().unwrap()) {
        setup(c.required("setup").unwrap());
        let path = node_path::to_native(
            c.required("configurationPath")
                .unwrap()
                .as_js_string()
                .unwrap(),
        );
        let actual = if c.required("operation").unwrap() == &J::from("file") {
            // Read boundary is tested with the same explicit account by first
            // parsing the original file; the production wrapper is checked below.
            match config_read(&path).await {
                Ok(input) => {
                    host_config::checked_host_config_with_context(input, path, context(&account))
                        .await
                }
                Err(error) => Err(error),
            }
        } else {
            host_config::checked_host_config_with_context(
                c.required("config").unwrap().clone(),
                path,
                context(&account),
            )
            .await
        };
        let actual = J::object([
            ("id", c.required("id").unwrap().clone()),
            ("result", result(actual)),
            (
                "tree",
                tree(&node_path::to_native(
                    c.required("root").unwrap().as_js_string().unwrap(),
                )),
            ),
        ]);
        records.push(actual.clone());
        if json::canonical(&actual, usize::MAX).unwrap() != json::canonical(e, usize::MAX).unwrap()
        {
            mismatches.push(J::object([
                ("id", c.required("id").unwrap().clone()),
                ("actual", actual),
                ("expected", e.clone()),
            ]));
        }
    }
    if let Some(path) = std::env::var_os("IVY_HOST_STARTUP_CORPUS") {
        fs::write(
            path,
            json::stringify(
                &J::object([
                    ("request", request),
                    ("expected", expected),
                    ("actual", J::Array(records)),
                ]),
                usize::MAX,
            )
            .unwrap(),
        )
        .unwrap();
    }
    println!("startup configuration differential cases: {}", cases.len());
    assert!(
        mismatches.is_empty(),
        "{}",
        json::stringify(&J::Array(mismatches), usize::MAX).unwrap()
    );
}
async fn config_read(path: &Path) -> IvyResult<J> {
    ivy_host_runtime::config::json_file(path.to_owned(), 1024 * 1024).await
}

#[tokio::test]
#[cfg(windows)]
async fn native_host_roots_retain_utf16_directory_entries() {
    use std::os::windows::ffi::OsStringExt;
    let root = Root::new();
    let mut config = base(&root.0);
    let opaque = std::ffi::OsString::from_wide(&[0xd800]);
    for (field, directory) in [
        ("runtimeRoot", "runtime"),
        ("artifactRoot", "artifacts"),
        ("stagingRoot", "staging"),
    ] {
        config.insert(field, jp(&root.0.join(directory).join(&opaque)));
    }
    let admitted = host_config::checked_host_config_with_context(
        config.clone(),
        root.0.join("host.json"),
        context(&root.0.join("account")),
    )
    .await
    .unwrap();
    for (field, directory) in [
        ("runtimeRoot", "runtime"),
        ("artifactRoot", "artifacts"),
        ("stagingRoot", "staging"),
    ] {
        assert_eq!(admitted.get(field), config.get(field));
        let entries = fs::read_dir(root.0.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![opaque.clone()]);
    }
    assert!(
        root.0
            .join("runtime")
            .join(&opaque)
            .join("instances")
            .join("fixture")
            .join("data")
            .is_dir()
    );
}

#[tokio::test]
async fn native_home_admission_preserves_shared_and_owned_storage_boundaries() {
    let root = Root::new();
    let malformed = [
        J::Null,
        false.into(),
        J::Number(0.0),
        J::Number(7.0),
        J::Array(vec![]),
        J::object([] as [(&str, J); 0]),
    ];
    for field in ["codexHome", "nativeHome", "appServer"] {
        for (index, value) in malformed.iter().enumerate() {
            if field == "appServer" && (value.is_null() || value.as_object().is_some()) {
                continue;
            }
            let r = root.0.join(format!("invalid-{field}-{index}"));
            // Supply the account default separately so this case reaches home
            // admission rather than failing while deriving a skills path.
            let mut settings = J::object([
                ("codexHome", val(&r, "shared")),
                ("skillsRoot", val(&r, "skills")),
            ]);
            settings.insert(field, value.clone());
            let mut config = base(&r);
            config.insert(
                "instances",
                J::Array(vec![instance("agent", "agent-manager", settings)]),
            );
            let error = host_config::checked_host_config_with_context(
                config,
                r.join("host.json"),
                context(&root.0.join("account")),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "invalid_arguments", "{field}/{index}");
            assert!(!r.join("runtime").join("instances").exists());
        }
    }
    for (index, mode) in ["owned-stdio", "claude-adapter", "external-proxy"]
        .iter()
        .enumerate()
    {
        let r = root.0.join(format!("owned-{index}"));
        let home = r
            .join("runtime")
            .join("instances")
            .join("agent")
            .join("data")
            .join("home");
        let mut config = base(&r);
        config.insert(
            "instances",
            J::Array(vec![instance(
                "agent",
                "agent-manager",
                J::object([
                    ("codexHome", jp(&home)),
                    ("appServer", J::object([("mode", (*mode).into())])),
                ]),
            )]),
        );
        let result = host_config::checked_host_config_with_context(
            config,
            r.join("host.json"),
            context(&root.0.join("account")),
        )
        .await;
        if *mode == "external-proxy" {
            assert_eq!(result.unwrap_err().code, "target_conflict");
            assert!(!r.join("runtime").join("instances").exists());
        } else {
            let admitted = result.unwrap();
            assert_eq!(
                admitted.required("instances").unwrap().as_array().unwrap()[0]
                    .required("settings")
                    .unwrap()
                    .get("codexHome"),
                Some(&jp(&home))
            );
            assert!(home.parent().unwrap().is_dir());
            assert!(!home.exists());
        }
    }
    for (name, expected, accepted) in malformed
        .into_iter()
        .enumerate()
        .map(|(index, expected)| (format!("external-invalid-{index}"), expected, false))
        .chain([
            ("external-shared".into(), val(&root.0, "shared-home"), true),
            (
                "overlap".into(),
                jp(&root.0.join("overlap").join("runtime").join("home")),
                false,
            ),
        ])
    {
        let r = root.0.join(name);
        let mut config = base(&r);
        config.insert(
            "instances",
            J::Array(vec![instance(
                "agent",
                "agent-manager",
                J::object([
                    ("codexHome", false.into()),
                    ("skillsRoot", val(&r, "skills")),
                    (
                        "appServer",
                        J::object([
                            ("mode", "external-proxy".into()),
                            ("expectedCodexHome", expected),
                        ]),
                    ),
                ]),
            )]),
        );
        let result = host_config::checked_host_config_with_context(
            config,
            r.join("host.json"),
            context(&root.0.join("account")),
        )
        .await;
        if accepted {
            assert!(result.is_ok());
            assert!(!root.0.join("shared-home").exists());
        } else {
            assert_eq!(result.unwrap_err().code, "target_conflict");
            assert!(!r.join("runtime").join("instances").exists());
        }
    }
}

#[tokio::test]
async fn production_host_loader_uses_the_executing_account_and_bounded_json_file() {
    let invalid_root = Root::new();
    let invalid_path = invalid_root.0.join("host.json");
    let mut invalid = base(&invalid_root.0);
    let mut instances = invalid
        .required("instances")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    let mut settings = instances[0].required("settings").unwrap().clone();
    settings.insert("text", "native-�".into());
    instances[0].insert("settings", settings);
    invalid.insert("instances", J::Array(instances));
    let mut bytes = json::stringify(&invalid, usize::MAX).unwrap().into_bytes();
    let offset = bytes
        .windows(3)
        .position(|part| part == b"\xef\xbf\xbd")
        .unwrap();
    bytes.splice(offset..offset + 3, [0xff]);
    fs::write(&invalid_path, &bytes).unwrap();
    assert_eq!(
        host_config::host_config(&invalid_path)
            .await
            .unwrap_err()
            .code,
        "internal_error"
    );
    assert!(!invalid_root.0.join("runtime").exists());
    assert_eq!(fs::read(&invalid_path).unwrap(), bytes);
    let root = Root::new();
    let path = root.0.join("host.json");
    let config = base(&root.0);
    fs::write(&path, json::stringify(&config, usize::MAX).unwrap()).unwrap();
    let loaded = host_config::host_config(&path).await.unwrap();
    let checked = host_config::checked_host_config(config.clone(), &path)
        .await
        .unwrap();
    super::assert_json_eq(&loaded, &checked);
    assert_eq!(
        loaded.required("runtimeRoot").unwrap(),
        config.required("runtimeRoot").unwrap()
    );
    assert!(root.0.join("runtime/instances/fixture/data").is_dir());
    fs::write(&path, "{}").unwrap();
    assert_eq!(
        host_config::host_config(&path).await.unwrap_err().code,
        "invalid_arguments"
    );
}

#[tokio::test]
async fn runtime_maintenance_markers_match_node_lstat_and_path_semantics() {
    let root = Root::new();
    let mut cases = Vec::new();
    for (i, kind) in [
        "missing",
        "file",
        "directory",
        "linked",
        "parent-file",
        "ivy-root",
        "null-ivy",
    ]
    .iter()
    .enumerate()
    {
        let r = root.0.join(i.to_string());
        let mut config = J::object([("runtimeRoot", val(&r, "runtime"))]);
        if *kind == "ivy-root" {
            config.insert("ivyRoot", val(&r, "ivy"));
        }
        if *kind == "null-ivy" {
            config.insert("ivyRoot", J::Null);
        }
        let marker = runtime_maintenance::runtime_reset_marker(&config).unwrap();
        let mut actions = Vec::new();
        match *kind {
            "file" | "ivy-root" | "null-ivy" => actions.push(file(jp(&marker), "{}")),
            "directory" => actions.push(action("directory", jp(&marker))),
            "linked" => {
                actions.push(action("directory", val(&r, "target")));
                actions.push(link(jp(&marker), val(&r, "target")));
            }
            "parent-file" => actions.push(file(jp(marker.parent().unwrap()), "file")),
            _ => (),
        }
        cases.push(J::object([
            ("id", kind.to_string().into()),
            ("config", config),
            ("dataRoot", val(&r, "data")),
            ("setup", J::Array(actions)),
        ]));
    }
    let expected = oracle(
        &J::object([
            ("kind", "maintenance".into()),
            ("cases", J::Array(cases.clone())),
        ]),
        &root.0,
    );
    for (c, e) in cases.iter().zip(expected.as_array().unwrap()) {
        let config = c.required("config").unwrap();
        let actual = J::object([
            ("id", c.required("id").unwrap().clone()),
            (
                "marker",
                result(runtime_maintenance::runtime_reset_marker(config).map(|v| jp(&v))),
            ),
            (
                "active",
                result(
                    runtime_maintenance::runtime_reset_active(config)
                        .await
                        .map(J::from),
                ),
            ),
            (
                "hive",
                result(Ok(jp(&runtime_maintenance::hive_reset_bootstrap_marker(
                    &node_path::to_native(c.required("dataRoot").unwrap().as_js_string().unwrap()),
                )))),
            ),
        ]);
        super::assert_json_eq_with_context(&actual, e, c);
    }
    #[cfg(unix)]
    {
        let config = J::object([("runtimeRoot", val(&root.0, "unix/runtime"))]);
        let marker = runtime_maintenance::runtime_reset_marker(&config).unwrap();
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let target = root.0.join("regular");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &marker).unwrap();
        assert!(
            !runtime_maintenance::runtime_reset_active(&config)
                .await
                .unwrap()
        );
        fs::remove_file(&target).unwrap();
        assert!(
            !runtime_maintenance::runtime_reset_active(&config)
                .await
                .unwrap()
        );
    }
}

#[test]
fn native_account_probe() {
    let Some(path) = std::env::var_os("IVY_ACCOUNT_PROBE") else {
        return;
    };
    let os_home = result(
        native_account::os_homedir()
            .map(J::String)
            .map_err(|_| ivy_contracts::IvyError::internal()),
    );
    let account = result(HostContext::current().map(|c| J::String(c.account_home)));
    let default = result(HostContext::current().map(|c| {
        J::String(node_path::join(
            Style::native(),
            &[c.account_home, ".ivy".into(), "config.json".into()],
        ))
    }));
    let selected = native_account::environment()
        .get(&JsString::from("IVY_ACCOUNT_UTF16"))
        .cloned()
        .map(J::String)
        .unwrap_or(J::Null);
    fs::write(
        path,
        json::stringify(
            &J::object([
                ("osHome", os_home),
                ("accountHome", account),
                ("defaultPath", default),
                ("selected", selected),
            ]),
            usize::MAX,
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn executing_account_environment_preserves_native_utf16_and_profile_selection() {
    let root = Root::new();
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = root.0.join("home");
    let mut cases = vec![
        ("profile", None, None),
        ("empty", Some(JsString::from("")), None),
        ("short-ascii", Some("x".into()), None),
        ("two-ascii", Some("xy".into()), None),
        ("short-unicode", Some("é".into()), None),
        ("unicode", Some("€".into()), None),
        ("relative", Some("relative/home".into()), None),
        (
            "absolute",
            Some(node_path::from_native(&home)),
            Some(home.clone()),
        ),
    ];
    #[cfg(windows)]
    {
        let opaque = home.join(node_path::to_native(&JsString(vec![0xd800])));
        cases.push((
            "opaque",
            Some(node_path::from_native(&opaque)),
            Some(opaque.clone()),
        ));
        let mut mixed = node_path::from_native(&home);
        mixed.0.extend([47, 0xd800]);
        cases.push(("opaque-mixed", Some(mixed), Some(opaque)));
        let mixed_case = root.0.join("mixed-case-home");
        cases.push((
            "mixed-case-key",
            Some(node_path::from_native(&mixed_case)),
            Some(mixed_case),
        ));
    }
    let selected = if cfg!(windows) {
        JsString(vec![65, 0xd800, 66])
    } else {
        JsString::from("A€B")
    };
    for (name, value, expected_account) in cases {
        let probe = root.0.join(format!("probe-{name}.json"));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "host_startup_cases::native_account_probe",
                "--exact",
                "--nocapture",
            ])
            .env_remove(key)
            .env("IVY_ACCOUNT_PROBE", &probe)
            .env("IVY_ACCOUNT_UTF16", node_path::to_native(&selected));
        if let Some(value) = &value {
            let key = if cfg!(windows) && name == "mixed-case-key" {
                "uSeRpRoFiLe"
            } else {
                key
            };
            command.env(key, node_path::to_native(value));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = json::parse(&fs::read_to_string(probe).unwrap(), usize::MAX).unwrap();
        let success = |field| {
            let item = actual.required(field).unwrap();
            assert_eq!(
                item.get("ok"),
                Some(&J::Bool(true)),
                "{name}/{field}: {item:?}"
            );
            item.required("value").unwrap().clone()
        };
        let os_home = success("osHome");
        if let Some(value) = &value {
            assert_eq!(os_home, J::String(value.clone()), "{name}");
        } else {
            assert!(node_path::to_native(os_home.as_js_string().unwrap()).is_absolute());
        }
        assert_eq!(
            actual.get("selected"),
            Some(&J::String(selected.clone())),
            "{name}"
        );
        let expected_account = if value.is_none() {
            Some(node_path::to_native(os_home.as_js_string().unwrap()))
        } else {
            expected_account
        };
        if let Some(account) = &expected_account {
            super::assert_json_eq_with_context(
                &success("accountHome"),
                &jp(account),
                &J::from(name),
            );
            super::assert_json_eq_with_context(
                &success("defaultPath"),
                &jp(&account.join(".ivy").join("config.json")),
                &J::from(name),
            );
            if value.is_some() {
                assert!(!account.exists(), "{name}: home was created");
            }
        } else {
            for field in ["accountHome", "defaultPath"] {
                let item = actual.required(field).unwrap();
                assert_eq!(
                    item.get("ok"),
                    Some(&J::Bool(false)),
                    "{name}/{field}: {item:?}"
                );
                assert_eq!(
                    item.required("error").unwrap().get("code"),
                    Some(&J::Number(-32602.0)),
                    "{name}/{field}: {item:?}"
                );
                let data = item.required("error").unwrap().required("data").unwrap();
                assert_eq!(
                    data.get("code"),
                    Some(&J::from("invalid_arguments")),
                    "{name}/{field}: {item:?}"
                );
                assert_eq!(data.get("outcome"), Some(&J::from("not_executed")));
            }
        }
    }
}

#[test]
fn native_filesystem_paths_keep_os_identity_without_creating_future_directories() {
    let root = Root::new();
    let mut cases = Vec::new();
    for (i, operation) in ["realpath", "future", "future"].iter().enumerate() {
        let r = root.0.join(i.to_string());
        let path = if i == 0 {
            r.join("actual")
        } else if i == 1 {
            r.join("alias/missing/leaf")
        } else {
            r.join("file/child")
        };
        let actions = vec![
            action("directory", val(&r, "actual")),
            link(val(&r, "alias"), val(&r, "actual")),
            file(val(&r, "file"), "fixture"),
        ];
        cases.push(J::object([
            ("id", i.to_string().into()),
            ("root", jp(&r)),
            ("path", jp(&path)),
            ("operation", operation.to_string().into()),
            ("setup", J::Array(actions)),
        ]));
    }
    let existing_name = if cfg!(windows) {
        JsString(vec![0xd800])
    } else {
        "α".into()
    };
    let missing_name = if cfg!(windows) {
        JsString(vec![0xd801])
    } else {
        "β".into()
    };
    for (i, operation) in ["realpath", "future"].iter().enumerate() {
        let r = root.0.join(format!("utf16-{i}"));
        let existing = r.join(node_path::to_native(&existing_name));
        let mut path = existing.clone();
        if i == 1 {
            path.push(node_path::to_native(&missing_name));
        }
        cases.push(J::object([
            ("id", format!("utf16-{i}").into()),
            ("root", jp(&r)),
            ("path", jp(&path)),
            ("operation", operation.to_string().into()),
            ("setup", J::Array(vec![action("directory", jp(&existing))])),
        ]));
    }
    for c in &cases {
        setup(c.required("setup").unwrap());
        let directory = node_path::to_native(c.required("root").unwrap().as_js_string().unwrap());
        let before = tree(&directory);
        let id = c.required("id").unwrap().as_str().unwrap();
        let expected = match id.as_str() {
            "0" => Ok(jp(&fs::canonicalize(directory.join("actual")).unwrap())),
            "1" => Ok(jp(&fs::canonicalize(directory.join("actual"))
                .unwrap()
                .join("missing")
                .join("leaf"))),
            "2" => Err(ivy_contracts::IvyError::internal()),
            _ => {
                let mut path =
                    fs::canonicalize(directory.join(node_path::to_native(&existing_name))).unwrap();
                if id == "utf16-1" {
                    path.push(node_path::to_native(&missing_name));
                }
                Ok(jp(&path))
            }
        };
        let path = node_path::to_native(c.required("path").unwrap().as_js_string().unwrap());
        let value = if c.required("operation").unwrap() == &J::from("realpath") {
            paths::realpath(&path)
                .map(|v| jp(&v))
                .map_err(|_| ivy_contracts::IvyError::internal())
        } else {
            paths::future_path(c.required("path").unwrap().as_js_string().unwrap()).map(J::String)
        };
        super::assert_json_eq_with_context(&result(value), &result(expected), c);
        assert_eq!(
            tree(&directory),
            before,
            "Path admission must not create or change entries"
        );
    }
    #[cfg(windows)]
    {
        let a = node_path::to_native(&JsString(vec![67, 58, 92, 0xd800]));
        let b = node_path::to_native(&JsString(vec![67, 58, 92, 0xd801]));
        assert!(!paths::inside(&a, &b).unwrap());
    }
    #[cfg(unix)]
    assert!(!paths::inside(Path::new("/root"), Path::new("/root/..\\child")).unwrap());
}
