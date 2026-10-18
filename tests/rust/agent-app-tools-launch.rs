#[path = "../../packages/host-runtime/rust/src/codex_app_tools.rs"]
mod app_tools;
use app_tools::{
    app_tools_pipe_candidates, installed_app_tools_pipe_candidates,
    latest_bundled_app_tools_server, resolve_app_tools_launch,
};
use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("ivy-app-tools-launch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn release(&self, name: &str, complete: bool) {
        let path = self.0.join(name);
        std::fs::create_dir(&path).unwrap();
        if complete {
            std::fs::write(path.join("server.mjs"), b"").unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ivy-app-tools-launch-")
        );
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn path(p: &std::path::Path) -> IvyJson {
    p.to_string_lossy().into_owned().into()
}

#[test]
fn bundled_and_explicit_launches_match_original_typescript() {
    let f = Fixture::new();
    for (name, complete) in [
        ("0.1.9", true),
        ("0.1.10", true),
        ("0.2.0", false),
        ("not-a-version", true),
        ("2.0003.4", true),
        ("2.3.3", true),
        ("99.0.0", false),
    ] {
        f.release(name, complete);
    }
    let outside = Fixture::new();
    let explicit = outside.0.join("custom.mjs");
    std::fs::write(&explicit, b"").unwrap();
    let node = f.0.join("old-node.exe");
    let old = f.0.join("0.1.9/server.mjs");
    let normalized = f.0.join("0.1.9/../0.1.10/server.mjs");
    let cases = vec![
        (None, None),
        (Some(node.clone()), None),
        (Some(node.clone()), Some(old)),
        (None, Some(normalized)),
        (None, Some(explicit.clone())),
        (Some(node), Some(explicit)),
        (None, Some(f.0.clone())),
    ];
    let input = IvyJson::object([
        ("root", path(&f.0)),
        (
            "cases",
            IvyJson::Array(
                cases
                    .iter()
                    .map(|(node, server)| {
                        let mut value = IvyJson::object([
                            ("pipePath", "auto".into()),
                            (
                                "actorThreadId",
                                "00000000-0000-0000-0000-000000000000".into(),
                            ),
                            ("serverHash", format!("sha256:{}", "0".repeat(64)).into()),
                        ]);
                        if let Some(p) = node {
                            value.insert("nodeExecutable", path(p));
                        }
                        if let Some(p) = server {
                            value.insert("serverPath", path(p));
                        }
                        value
                    })
                    .collect(),
            ),
        ),
    ]);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg(root.join("tests/fixtures/app-tools-launch-oracle.mjs"))
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stringify(&input, 1024 * 1024).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oracle = parse(std::str::from_utf8(&output.stdout).unwrap(), 1024 * 1024).unwrap();
    let runtime = PathBuf::from(oracle.get("runtimeNode").unwrap().as_str().unwrap());
    let mut results = Vec::new();
    for (node, server) in cases {
        let spec = resolve_app_tools_launch(
            node.as_deref(),
            server.as_deref(),
            &runtime,
            &f.0,
            vec!["\\\\.\\pipe\\fixture".into()],
        )
        .unwrap();
        assert_eq!(spec.args.len(), 1);
        assert_eq!(spec.installation_identity.as_os_str(), spec.args[0]);
        assert_eq!(spec.cwd, spec.installation_identity.parent().unwrap());
        assert!(spec.env.is_empty());
        let env = spec
            .environment_for_pipe(&"\\\\.\\pipe\\fixture".into())
            .unwrap();
        assert_eq!(
            env.get(&JsString::from("CODEX_APP_TOOLS_PIPE_PATH")),
            Some(&JsString::from("\\\\.\\pipe\\fixture"))
        );
        assert_eq!(
            spec.environment_for_pipe(&"foreign".into())
                .unwrap_err()
                .code,
            "app_tools_pipe_unavailable"
        );
        results.push(IvyJson::object([
            ("nodeExecutable", path(&spec.executable)),
            ("serverPath", path(&spec.installation_identity)),
        ]));
    }
    assert_eq!(
        IvyJson::Array(results),
        oracle.get("results").unwrap().clone()
    );
    assert_eq!(
        latest_bundled_app_tools_server(&f.0.join("0.1.9/..")).unwrap(),
        f.0.join("2.0003.4/server.mjs")
    );
    std::fs::write(f.0.join("99.0.0/server.mjs"), b"").unwrap();
    assert_eq!(
        latest_bundled_app_tools_server(&f.0).unwrap(),
        f.0.join("99.0.0/server.mjs")
    );
}
#[test]
fn incomplete_installation_and_pipe_order_do_not_invent_candidates() {
    let f = Fixture::new();
    f.release("1.0.0", false);
    assert_eq!(
        latest_bundled_app_tools_server(&f.0).unwrap_err().code,
        "app_tools_unavailable"
    );
    let current = JsString::from("\\\\.\\pipe\\codex-browser-use-b");
    let names = vec![
        "other".into(),
        "codex-browser-use-a".into(),
        "codex-browser-use-b".into(),
        "codex-browser-use-a".into(),
    ];
    assert_eq!(
        app_tools_pipe_candidates(&"auto".into(), Some(&current), names),
        vec![current, JsString::from("\\\\.\\pipe\\codex-browser-use-a")]
    );
    assert!(app_tools_pipe_candidates(&"auto".into(), Some(&"invalid".into()), []).is_empty());
    assert_eq!(
        installed_app_tools_pipe_candidates(&"\\\\.\\pipe\\explicit".into()),
        vec![JsString::from("\\\\.\\pipe\\explicit")]
    );
    let lone = JsString(vec![0xd800]);
    assert_eq!(app_tools_pipe_candidates(&lone, None, []), vec![lone]);
}
