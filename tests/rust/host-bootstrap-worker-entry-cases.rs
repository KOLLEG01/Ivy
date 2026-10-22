use super::ServiceFixture;
use ivy_contracts::{IvyJson, JsString, json};
use ivy_host_executor::worker::WorkerPaths;
use ivy_host_runtime::node_path::{self, Context};
use std::{collections::BTreeMap, ffi::OsString, path::Path, process::Command};

#[test]
fn worker_arguments_match_the_original_strict_node_entry() {
    let fixture = ServiceFixture::new();
    let node = std::env::var_os("IVY_TEST_NODE").expect("Pinned Node supplied by the test runner");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/host-bootstrap-worker-entry-cases.mjs");
    let original = Command::new(node)
        .arg(script)
        .arg(&fixture.0)
        .output()
        .unwrap();
    assert!(
        original.status.success(),
        "{}",
        String::from_utf8_lossy(&original.stderr)
    );
    let rows = json::parse(std::str::from_utf8(&original.stdout).unwrap(), 64 * 1024).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 20);
    let context = Context {
        style: node_path::Style::native(),
        cwd: node_path::from_native(&fixture.0),
    };
    for row in rows {
        let args = row
            .required("args")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|value| node_path::to_native(value.as_js_string().unwrap()).into_os_string())
            .collect::<Vec<_>>();
        match WorkerPaths::parse(&args, &context) {
            Ok(paths) => {
                assert_eq!(row.get("code"), None, "{args:?}");
                let actual = IvyJson::object([
                    (
                        "config",
                        IvyJson::String(node_path::from_native(&paths.config)),
                    ),
                    (
                        "request",
                        IvyJson::String(node_path::from_native(&paths.request)),
                    ),
                    (
                        "distribution",
                        IvyJson::String(node_path::from_native(&paths.distribution)),
                    ),
                ]);
                assert_eq!(&actual, row.required("paths").unwrap(), "{args:?}");
                assert!(paths.node_executable.is_none());
            }
            Err(error) => assert_eq!(
                Some(&IvyJson::String(error.code_js())),
                row.get("code"),
                "{args:?}"
            ),
        }
    }
}

#[test]
fn private_workload_input_keeps_absolute_file_admission() {
    let context = Context::current().unwrap();
    let args = [
        "--config=c",
        "--request=r",
        "--distribution=d",
        "--node-executable=relative-node",
    ]
    .map(OsString::from);
    let paths = WorkerPaths::parse(&args, &context).unwrap();
    assert_eq!(paths.node_executable, Some("relative-node".into()));
    assert!(paths.config.is_absolute());
    assert!(paths.request.is_absolute());
    assert!(paths.distribution.is_absolute());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = runtime
        .block_on(ivy_host_executor::worker::run(&args))
        .unwrap_err();
    assert_eq!(error.code, "unsupported_runtime");
    assert_eq!(error.outcome, ivy_host_runtime::Outcome::NotExecuted);
}

#[test]
fn native_worker_error_exit_precedes_node_and_os_effects() {
    for (args, code) in [
        (vec![], "invalid_arguments"),
        (vec!["--unknown"], "internal_error"),
        (
            vec![
                "--config=c",
                "--request=r",
                "--distribution=d",
                "--node-executable=relative-node",
            ],
            "unsupported_runtime",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ivy-bootstrap-auto-worker"))
            .args(args)
            .env_remove("IVY_NODE_EXECUTABLE")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = json::parse(std::str::from_utf8(&output.stderr).unwrap(), 1024).unwrap();
        assert_eq!(
            error,
            IvyJson::object([
                ("code", code.into()),
                (
                    "message",
                    "Automatic bootstrap update needs attention.".into()
                ),
            ])
        );
    }
}

#[cfg(windows)]
#[test]
fn native_worker_path_values_retain_utf16_units() {
    use std::os::windows::ffi::OsStringExt;
    let context = Context::current().unwrap();
    let mut request = JsString::from("--request=").0;
    request.extend([0xd800, 0x61]);
    let args = [
        "--config=c".into(),
        OsString::from_wide(&request),
        "--distribution=d".into(),
    ];
    let parsed = WorkerPaths::parse(&args, &context).unwrap();
    assert!(
        node_path::from_native(&parsed.request)
            .0
            .ends_with(&[0xd800, 0x61])
    );
}
