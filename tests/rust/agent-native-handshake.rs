use ivy_host_runtime::{codex_rpc, node_path, paths};
#[path = "../../packages/host-runtime/rust/src/codex_handshake.rs"]
mod handshake;
use ivy_contracts::{
    IvyJson, JsString,
    json::{parse, stringify},
};
use std::{
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
fn handshake_version_matches_original_grammar_including_utf16_and_ecmascript_whitespace() {
    let mut values = vec![
        IvyJson::Null,
        true.into(),
        IvyJson::Number(123.0),
        IvyJson::Array(vec![]),
        IvyJson::object([] as [(&str, IvyJson); 0]),
    ];
    for prefix in [
        "codex",
        "codex app-server",
        "",
        " codex",
        "codex ",
        "codex  app",
        "codex\tapp",
        "codex/extra",
        "codex_ä",
    ] {
        for version in [
            "0.154.0",
            "00.154.00",
            "1.2",
            "1.2.3.4",
            "1.2.3-beta",
            "1.2.3 other",
            "1.2.3\nother",
            "1.2.3/other",
        ] {
            values.push(format!("{prefix}/{version}").into());
        }
    }
    for unit in [
        9, 10, 11, 12, 13, 32, 0x85, 0xa0, 0x1680, 0x180e, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004,
        0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x200b, 0x2028, 0x2029, 0x202f, 0x205f,
        0x3000, 0xfeff, 0xd800, 0xdc00,
    ] {
        let mut suffix = JsString::from("codex/1.2.3");
        suffix.0.push(unit);
        suffix.0.push(65);
        values.push(IvyJson::String(suffix));
        let mut prefix = JsString::from("codex");
        prefix.0.push(unit);
        prefix.0.extend(JsString::from("app/1.2.3").0);
        values.push(IvyJson::String(prefix));
    }
    let expected_input = IvyJson::Array(values.clone());
    let actual = IvyJson::Array(
        values
            .iter()
            .map(|v| match handshake::initialized_server_version(v) {
                Ok(version) => IvyJson::object([("version", IvyJson::String(version))]),
                Err(error) => IvyJson::object([
                    ("code", error.code.into()),
                    ("message", error.message.into()),
                ]),
            })
            .collect(),
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("node")
        .arg(root.join("tests/fixtures/native-handshake-oracle.mjs"))
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
        .write_all(stringify(&expected_input, 65536).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        actual,
        parse(std::str::from_utf8(&output.stdout).unwrap(), 65536).unwrap()
    );
}

#[tokio::test]
async fn handshake_checks_identity_before_initialized_notification() {
    let root = std::env::temp_dir().join(format!("ivy-agent-handshake-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let expected = paths::realpath(&root).unwrap();
    let home = IvyJson::from(root.to_str().unwrap());
    let catalog = parse(
        include_str!("../../specs/native/codex-0.154.0/catalog.json"),
        2 * 1024 * 1024,
    )
    .unwrap();
    for (result, code) in [
        (
            IvyJson::object([
                ("codexHome", home.clone()),
                ("userAgent", "codex app-server/0.154.0 Windows".into()),
            ]),
            None,
        ),
        (
            IvyJson::object([
                ("codexHome", home.clone()),
                ("userAgent", "codex/0.153.0".into()),
            ]),
            Some("native_server_version_mismatch"),
        ),
        (
            IvyJson::object([("codexHome", home.clone()), ("userAgent", "codex".into())]),
            Some("native_server_version_unavailable"),
        ),
        (
            IvyJson::object([
                ("codexHome", true.into()),
                ("userAgent", "codex/0.154.0".into()),
            ]),
            Some("native_home_mismatch"),
        ),
        (
            IvyJson::object([
                ("codexHome", std::env::temp_dir().to_str().unwrap().into()),
                ("userAgent", "codex/0.154.0".into()),
            ]),
            Some("native_home_mismatch"),
        ),
        ("rpc-error".into(), Some("native_initialization_failed")),
        (IvyJson::Null, Some("internal_error")),
    ] {
        let (input, read) = tokio::io::duplex(65536);
        let (mut write, output) = tokio::io::duplex(65536);
        let rpc = codex_rpc::NativeRpc::start(&catalog, input, output, Default::default()).unwrap();
        let connected = rpc.clone();
        let expected = expected.clone();
        let pending = tokio::spawn(async move {
            handshake::initialize(
                &connected,
                handshake::Handshake {
                    client_name: &"Ivy".into(),
                    client_version: &"1.2.3".into(),
                    native_version: &"0.154.0".into(),
                    expected_home: &expected,
                },
            )
            .await
        });
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        let request = parse(&line, 65536).unwrap();
        assert_eq!(request.get("method"), Some(&"initialize".into()));
        assert_eq!(request.get("params"),Some(&parse(r#"{"clientInfo":{"name":"Ivy","version":"1.2.3"},"capabilities":{"experimentalApi":true,"requestAttestation":false}}"#,65536).unwrap()));
        let mut reply = IvyJson::object([("id", request.get("id").unwrap().clone())]);
        if result == IvyJson::from("rpc-error") {
            reply.insert(
                "error",
                IvyJson::object([
                    ("code", IvyJson::Number(-32603.0)),
                    ("message", "unavailable".into()),
                ]),
            );
        } else {
            reply.insert("result", result);
        }
        write
            .write_all(format!("{}\n", stringify(&reply, 65536).unwrap()).as_bytes())
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.err().as_ref().map(|e| e.code.as_str()), code);
        line.clear();
        if code.is_none() {
            read.read_line(&mut line).await.unwrap();
            assert_eq!(
                parse(&line, 65536).unwrap().get("method"),
                Some(&"initialized".into())
            );
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), read.read_line(&mut line))
                    .await
                    .is_err()
            );
        }
        rpc.close("test_complete").await;
    }
    std::fs::remove_dir(&root).unwrap();
}
